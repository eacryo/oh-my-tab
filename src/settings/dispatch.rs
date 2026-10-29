//! Control-event dispatch, live CONFIG writes, and dependent enable/refresh logic.

use super::sidebar;
use super::*;
use crate::callback_guard;

// Every change is written to the in-memory CONFIG immediately with a debounced disk write;
// no pending/staged state exists anymore. Numeric text fields may hold transient invalid
// values while typing (applied only when valid); sliders take effect live while drags
// persist with a debounce.

/// Editable control ids. The pointer → field mapping lives in control_field_of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlField {
    Theme,
    PanelMaterial,
    GlassStyle,
    Locale,
    LogLevel,
    LaunchAtLogin,
    HideMenuBarIcon,
    WindowsEnabled,
    ShowMinimized,
    ShowHiddenAppWindows,
    ShowOtherDesktops,
    ThumbnailsEnabled,
    FocusedThumbnailPrewarm,
    ShowAppNameInCards,
    ShowFooterTitle,
    CardTextSize,
    ThumbnailSize,
    StatusBarTextSize,
    OverlayPosition,
    ActivationMode,
    PanelCloseEnabled,
    PanelMinimizeEnabled,
    PanelHideEnabled,
    PanelQuitEnabled,
    CornerRadius,
    Modifier,
    KeystrokeDisplayEnabled,
    KeystrokeDisplayMode,
    KeystrokeDisplayTapLevel,
    KeystrokeDisplayPosition,
    KeystrokeDisplayInitialPosition,
    KeystrokeDisplayMaxExtent,
    MouseEnabled,
    ReverseScroll,
    ScrollMode,
    LineCount,
    DisablePointerAccel,
    PointerAcceleration,
    SmoothScrollingPreset,
    SmoothScrollingResponse,
    SmoothScrollingSpeed,
    SmoothScrollingAcceleration,
    SmoothScrollingInertia,
    MappingEnabled,
    ClipboardEnabled,
    ClipboardPaused,
    ClipboardClearOnQuit,
    ClipboardSeparateClearAll,
    ClipboardShowSourceApp,
    ClipboardMoveUsedToTop,
    ClipboardDeleteAfterPaste,
    ClipboardClearSystemPasteboardAfterPaste,
    ClipboardMaxEntries,
    ClipboardAutoExpireDays,
    ClipboardImageRows,
    ClipboardPinFollow,
    ClipboardPickerPlacement,
    // The Key Bindings page's two group switches. The per-binding rows are dynamic and dispatch
    // through their own sender-pointer lookups (`handleBindingToggle:`/`handleBindingDelete:`), not
    // through this static control table.
    KeyBindingsEnabled,
    UpdateAutoCheck,
    UpdateAutoDownload,
    // Finder Menu page. The two vec-backed lists carry the switch's catalog index: the control's
    // identity is its entry in FINDER_MENU_FILE_TYPES / finder_menu::terminals(), and the config
    // value is rebuilt in that order rather than in click order.
    FinderMenuEnabled,
    FinderMenuLayout,
    FinderMenuNewFileType(usize),
    FinderMenuTerminal(usize),
    FinderMenuCopyPath,
}

/// Identify a field by its control pointer (the window is reused, pointers are stable).
unsafe fn control_field_of(sender: *mut AnyObject) -> Option<ControlField> {
    with_settings_ui(|ui| {
        let u = ui.as_ref()?;
        let ptr = sender as usize;
        let m = |ctrl: *mut AnyObject, field: ControlField| (ptr == ctrl as usize).then_some(field);
        m(u.theme, ControlField::Theme)
            .or_else(|| m(u.panel_material, ControlField::PanelMaterial))
            .or_else(|| m(u.glass_style, ControlField::GlassStyle))
            .or_else(|| m(u.locale, ControlField::Locale))
            .or_else(|| m(u.log_level, ControlField::LogLevel))
            .or_else(|| m(u.launch_at_login, ControlField::LaunchAtLogin))
            .or_else(|| m(u.hide_menu_bar_icon, ControlField::HideMenuBarIcon))
            .or_else(|| m(u.windows_enabled, ControlField::WindowsEnabled))
            .or_else(|| m(u.show_minimized, ControlField::ShowMinimized))
            .or_else(|| {
                m(
                    u.show_hidden_app_windows,
                    ControlField::ShowHiddenAppWindows,
                )
            })
            .or_else(|| m(u.show_other_desktops, ControlField::ShowOtherDesktops))
            .or_else(|| m(u.thumbnails_enabled, ControlField::ThumbnailsEnabled))
            .or_else(|| m(u.thumbnail_size, ControlField::ThumbnailSize))
            .or_else(|| {
                m(
                    u.focused_thumbnail_prewarm,
                    ControlField::FocusedThumbnailPrewarm,
                )
            })
            .or_else(|| m(u.show_app_name_in_cards, ControlField::ShowAppNameInCards))
            .or_else(|| m(u.show_footer_title, ControlField::ShowFooterTitle))
            .or_else(|| m(u.card_text_size, ControlField::CardTextSize))
            .or_else(|| m(u.status_bar_text_size, ControlField::StatusBarTextSize))
            .or_else(|| m(u.overlay_position, ControlField::OverlayPosition))
            .or_else(|| m(u.activation_mode, ControlField::ActivationMode))
            .or_else(|| m(u.panel_close_enabled, ControlField::PanelCloseEnabled))
            .or_else(|| m(u.panel_minimize_enabled, ControlField::PanelMinimizeEnabled))
            .or_else(|| m(u.panel_hide_enabled, ControlField::PanelHideEnabled))
            .or_else(|| m(u.panel_quit_enabled, ControlField::PanelQuitEnabled))
            .or_else(|| m(u.corner_radius, ControlField::CornerRadius))
            .or_else(|| m(u.modifier, ControlField::Modifier))
            .or_else(|| {
                m(
                    u.keystroke_display_enabled,
                    ControlField::KeystrokeDisplayEnabled,
                )
            })
            .or_else(|| m(u.keystroke_display_mode, ControlField::KeystrokeDisplayMode))
            .or_else(|| {
                m(
                    u.keystroke_display_tap_level,
                    ControlField::KeystrokeDisplayTapLevel,
                )
            })
            .or_else(|| {
                m(
                    u.keystroke_display_position,
                    ControlField::KeystrokeDisplayPosition,
                )
            })
            .or_else(|| {
                m(
                    u.keystroke_display_initial_position,
                    ControlField::KeystrokeDisplayInitialPosition,
                )
            })
            .or_else(|| {
                m(
                    u.keystroke_display_max_extent,
                    ControlField::KeystrokeDisplayMaxExtent,
                )
            })
            .or_else(|| m(u.enable_mouse, ControlField::MouseEnabled))
            .or_else(|| m(u.reverse_scroll, ControlField::ReverseScroll))
            .or_else(|| m(u.scroll_mode, ControlField::ScrollMode))
            .or_else(|| m(u.line_count, ControlField::LineCount))
            .or_else(|| m(u.disable_pointer_accel, ControlField::DisablePointerAccel))
            .or_else(|| m(u.pointer_accel_slider, ControlField::PointerAcceleration))
            .or_else(|| {
                m(
                    u.smooth_scrolling_preset,
                    ControlField::SmoothScrollingPreset,
                )
            })
            .or_else(|| {
                m(
                    u.smooth_scrolling_response,
                    ControlField::SmoothScrollingResponse,
                )
            })
            .or_else(|| m(u.smooth_scrolling_speed, ControlField::SmoothScrollingSpeed))
            .or_else(|| {
                m(
                    u.smooth_scrolling_acceleration,
                    ControlField::SmoothScrollingAcceleration,
                )
            })
            .or_else(|| {
                m(
                    u.smooth_scrolling_inertia,
                    ControlField::SmoothScrollingInertia,
                )
            })
            .or_else(|| m(u.mapping_enabled, ControlField::MappingEnabled))
            .or_else(|| m(u.clipboard_enabled, ControlField::ClipboardEnabled))
            .or_else(|| m(u.clipboard_paused, ControlField::ClipboardPaused))
            .or_else(|| {
                m(
                    u.clipboard_clear_on_quit,
                    ControlField::ClipboardClearOnQuit,
                )
            })
            .or_else(|| {
                m(
                    u.clipboard_separate_clear_all,
                    ControlField::ClipboardSeparateClearAll,
                )
            })
            .or_else(|| {
                m(
                    u.clipboard_show_source_app,
                    ControlField::ClipboardShowSourceApp,
                )
            })
            .or_else(|| {
                m(
                    u.clipboard_move_used_to_top,
                    ControlField::ClipboardMoveUsedToTop,
                )
            })
            .or_else(|| {
                m(
                    u.clipboard_delete_after_paste,
                    ControlField::ClipboardDeleteAfterPaste,
                )
            })
            .or_else(|| {
                m(
                    u.clipboard_clear_system_pasteboard_after_paste,
                    ControlField::ClipboardClearSystemPasteboardAfterPaste,
                )
            })
            .or_else(|| m(u.clipboard_max_entries, ControlField::ClipboardMaxEntries))
            .or_else(|| {
                m(
                    u.clipboard_auto_expire_days,
                    ControlField::ClipboardAutoExpireDays,
                )
            })
            .or_else(|| m(u.clipboard_image_rows, ControlField::ClipboardImageRows))
            .or_else(|| m(u.clipboard_pin_follow, ControlField::ClipboardPinFollow))
            .or_else(|| {
                m(
                    u.clipboard_picker_placement,
                    ControlField::ClipboardPickerPlacement,
                )
            })
            .or_else(|| m(u.key_bindings_enabled, ControlField::KeyBindingsEnabled))
            .or_else(|| m(u.update_auto_check, ControlField::UpdateAutoCheck))
            .or_else(|| m(u.update_auto_download, ControlField::UpdateAutoDownload))
            .or_else(|| m(u.finder_menu_enabled, ControlField::FinderMenuEnabled))
            .or_else(|| m(u.finder_menu_layout, ControlField::FinderMenuLayout))
            .or_else(|| {
                u.finder_menu_new_file_switches
                    .iter()
                    .position(|ctrl| *ctrl as usize == ptr)
                    .map(ControlField::FinderMenuNewFileType)
            })
            .or_else(|| {
                u.finder_menu_terminal_switches
                    .iter()
                    .position(|ctrl| *ctrl as usize == ptr)
                    .map(ControlField::FinderMenuTerminal)
            })
            .or_else(|| m(u.finder_menu_copy_path, ControlField::FinderMenuCopyPath))
    })
}

pub(super) unsafe fn set_clipboard_shortcut_capture_state(
    ui: &SettingsUi,
    current: &str,
    recording: bool,
    error: Option<&str>,
) {
    if ui.clipboard_shortcut.is_null() {
        return;
    }
    let title = if recording {
        t("settings.clipboard_shortcut_record_prompt")
    } else {
        crate::mouse::shortcut::display_shortcut(current)
    };
    set_control_title(ui.clipboard_shortcut, &title);
    let mut button_frame: NSRect = msg_send![ui.clipboard_shortcut, frame];
    let Some(row_label) = SettingsRow::label_for(ui.clipboard_shortcut) else {
        return;
    };
    let row_label_frame: NSRect = msg_send![row_label, frame];
    // The LABEL's centre is not the ROW's centre: `center_label` adds
    // `text_field_line_height_correction` so the glyphs sit optically centred, and that correction
    // (1.30pt here) moves the label's frame away from the row's midline. The row's midline is the
    // label's baseline position plus the correction the row itself applied, i.e. the uncorrected
    // centre -- which is what a control centred in the row must use. Using the corrected centre
    // pushed the button 1.30pt off and made `--smoke-settings-layout` fail for a correct panel.
    let correction = crate::ffi::text_field_line_height_correction(row_label);
    let row_center = row_label_frame.origin.y + row_label_frame.size.height / 2.0 - correction;
    if error.is_some() && !ui.clipboard_shortcut_error.is_null() {
        let mut error_frame: NSRect = msg_send![ui.clipboard_shortcut_error, frame];
        let row_origin = row_center - ui.clipboard_shortcut_row_height / 2.0;
        error_frame.origin.y = row_origin + 2.0;
        let _: () = msg_send![ui.clipboard_shortcut_error, setFrame: error_frame];
        button_frame.origin.y = error_frame.origin.y + error_frame.size.height + 2.0;
    } else {
        button_frame.origin.y = row_center - button_frame.size.height / 2.0;
    }
    let _: () = msg_send![ui.clipboard_shortcut, setFrame: button_frame];
    if !ui.clipboard_shortcut_error.is_null() {
        set_field(ui.clipboard_shortcut_error, error.unwrap_or_default());
        let _: () = msg_send![ui.clipboard_shortcut_error, setHidden: error.is_none()];
    }
}

pub(super) fn set_clipboard_shortcut_recording_ui(recording: bool, error: Option<&str>) {
    let current = CONFIG.read().unwrap().clipboard.shortcut.clone();
    with_settings_ui(|slot| {
        let Some(ui) = slot.as_ref() else { return };
        let visible: bool = unsafe { msg_send![ui.window, isVisible] };
        if visible {
            unsafe { set_clipboard_shortcut_capture_state(ui, &current, recording, error) };
        }
    });
}

pub(super) fn refresh_clipboard_shortcut_record_control() {
    let recording = *REC_STAGE.lock().unwrap() != RecStage::Idle
        && *REC_MODE.lock().unwrap() == RecMode::ClipboardShortcut;
    set_clipboard_shortcut_recording_ui(recording, None);
}

/// Re-read the "separate clear all" switch from CONFIG. A config reload (file edit, defaults
/// restore) can flip the field while the settings window stays open; without this the switch
/// shows the old value while the picker header already follows the new one.
pub(super) fn refresh_clipboard_separate_clear_all_switch_from_config() {
    let on = CONFIG.read().unwrap().clipboard.separate_clear_all;
    with_settings_ui(|ui| {
        if let Some(u) = ui.as_ref() {
            unsafe {
                let _: () = msg_send![
                    u.clipboard_separate_clear_all,
                    setState: if on { 1isize } else { 0isize }
                ];
            }
        }
    });
}

pub(super) fn apply_recorded_clipboard_shortcut(shortcut: &str) {
    let old = CONFIG.read().unwrap().clone();
    let mut updated = old.clone();
    updated.clipboard.shortcut = shortcut.to_string();
    if let Ok(mut config) = CONFIG.write() {
        *config = updated.clone();
    }
    schedule_config_persist();
    apply_config_change(&old, &updated, ConfigChangeSource::Settings);
}

/// The settings-side owner of the panel size preview.
///
/// A slider drag produces one action per mouse-dragged event, but only the first should show the
/// panel and lift its window level, and the preview has to end exactly once -- on mouse-up, on a
/// lost pointer, or when the window or the feature goes away. This keeps that small state machine
/// in one place instead of threading flags through the control callback.
#[derive(Default)]
struct MaxExtentPreview {
    active: bool,
}

thread_local! {
    static MAX_EXTENT_PREVIEW: RefCell<MaxExtentPreview> = RefCell::new(MaxExtentPreview::default());
}

/// Show (or resize) the panel preview at `percent`. Called from the slider action, so it runs for
/// every step of the drag.
pub(super) fn preview_max_extent(percent: u32) {
    if !crate::is_main_thread() {
        return;
    }
    // Only a pointer gesture gets a preview. Keyboard and accessibility changes (arrow keys on the
    // focused slider, AX increment) also post this action, but they have no release to end a
    // preview with -- starting one there would leave the panel up with nothing to take it down.
    if !left_mouse_button_is_down() {
        return;
    }
    // begin() is idempotent: it only shows and raises on the first call of a drag. The flag here
    // only records that an end is owed, so repeated steps do not repeatedly tear the preview down.
    MAX_EXTENT_PREVIEW.with(|preview| preview.borrow_mut().active = true);
    crate::keystroke_display::begin_max_extent_preview(percent);
}

/// Whether the left mouse button is physically down right now. `NSEvent` reports the hardware
/// state, so it is true both inside AppKit drag tracking and outside a window event dispatch.
pub(crate) fn left_mouse_button_is_down() -> bool {
    const LEFT_MOUSE_BUTTON_MASK: usize = 1;
    unsafe {
        let pressed: usize = msg_send![class!(NSEvent), pressedMouseButtons];
        pressed & LEFT_MOUSE_BUTTON_MASK != 0
    }
}

/// End the preview if one is running, and put the control back in step with what was actually
/// committed. Safe to call repeatedly and from every teardown path.
///
/// The resync is not cosmetic: NSSlider keeps reporting the position the pointer last tracked,
/// while the action handler snaps the *committed* value to a whole ten. After a track click or a
/// double-click reset those differ (the knob can read 57 while 50 was committed and is what the
/// readout shows), and without this the knob stays where the pointer left it.
pub(crate) fn end_max_extent_preview() {
    if !crate::is_main_thread() {
        return;
    }
    let was_active = MAX_EXTENT_PREVIEW.with(|preview| {
        let mut preview = preview.borrow_mut();
        let was = preview.active;
        preview.active = false;
        was
    });
    if was_active {
        crate::keystroke_display::end_max_extent_preview();
    }
}

/// Bind a control to the unified change callback (switches/popups/sliders).
pub(super) unsafe fn bind_control(target: *mut AnyObject, ctrl: *mut AnyObject) {
    let _: () = msg_send![ctrl, setTarget: target];
    let _: () = msg_send![ctrl, setAction: sel!(handleControlChanged:)];
}

/// Whether a Finder Menu switch is on. A control that was never built counts as off, so reading
/// the list can never dereference a null pointer.
unsafe fn finder_menu_switch_on(control: *mut AnyObject) -> bool {
    if control.is_null() {
        return false;
    }
    let state: isize = msg_send![control, state];
    state == 1
}

/// Rebuild a vec-backed config list from the page's switch states, in the identity table's order.
///
/// The checkbox decides membership only: the ids are the catalog (file types or terminal bundle
/// ids) and the result follows it, never the order boxes were ticked, because the menu renders the
/// stored vec as its item order.
fn checked_ids<'a>(ids: impl IntoIterator<Item = &'a str>, states: &[bool]) -> Vec<String> {
    ids.into_iter()
        .zip(states.iter().copied())
        .filter(|(_, checked)| *checked)
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Fill every Finder Menu control from the config and settle the dependent enable states.
///
/// Shared by the ordinary open fill and the in-place config reload so the two cannot disagree
/// about what the page shows: a switch is membership of its id in the config vec, in catalog order.
pub(super) unsafe fn fill_finder_menu_controls(ui: &SettingsUi, cfg: &Config) {
    set_field(
        ui.finder_menu_status,
        super::finder_menu_status_text_now(crate::finder_menu::extension_enabled()),
    );
    let layout_idx = crate::config::FINDER_MENU_LAYOUTS
        .iter()
        .position(|layout| *layout == cfg.finder_menu.layout.as_str())
        .unwrap_or(0) as isize;
    let _: () = msg_send![ui.finder_menu_layout, selectItemAtIndex: layout_idx];
    let _: () = msg_send![
        ui.finder_menu_enabled,
        setState: if cfg.finder_menu.enabled { 1isize } else { 0isize }
    ];
    for (index, control) in ui.finder_menu_new_file_switches.iter().enumerate() {
        let checked = crate::config::FINDER_MENU_FILE_TYPES
            .get(index)
            .is_some_and(|extension| {
                cfg.finder_menu
                    .new_file_types
                    .iter()
                    .any(|value| value == extension)
            });
        let _: () = msg_send![*control, setState: if checked { 1isize } else { 0isize }];
    }
    for (index, control) in ui.finder_menu_terminal_switches.iter().enumerate() {
        let checked = crate::finder_menu::terminals()
            .get(index)
            .is_some_and(|spec| {
                cfg.finder_menu
                    .terminals
                    .iter()
                    .any(|bundle_id| bundle_id == spec.bundle_id)
            });
        let _: () = msg_send![*control, setState: if checked { 1isize } else { 0isize }];
    }
    let _: () = msg_send![
        ui.finder_menu_copy_path,
        setState: if cfg.finder_menu.copy_path {
            1isize
        } else {
            0isize
        }
    ];
    update_finder_menu_controls_enabled(ui);
}

/// Freeze the Finder Menu options below the master switch, and keep each terminal row gated by
/// whether that app is installed. The page build also calls this after creating the rows.
pub(super) unsafe fn update_finder_menu_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.finder_menu_enabled, state];
    let on = state == 1;
    let master_tooltip = t("settings.finder_menu.tooltip_disabled");
    let mut controls = vec![ui.finder_menu_layout, ui.finder_menu_copy_path];
    controls.extend(ui.finder_menu_new_file_switches.iter().copied());
    for ctrl in controls {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &master_tooltip);
    }
    let not_installed = t("settings.finder_menu.terminal_not_installed");
    for (index, control) in ui.finder_menu_terminal_switches.iter().enumerate() {
        let installed = crate::finder_menu::terminals()
            .get(index)
            .is_some_and(|spec| crate::finder_menu::terminal_installed(spec.bundle_id));
        if !on {
            SettingsRow::set_enabled_with_tooltip(*control, false, &master_tooltip);
        } else if installed {
            SettingsRow::set_enabled_with_tooltip(*control, true, "");
        } else {
            SettingsRow::set_enabled_with_tooltip(*control, false, &not_installed);
        }
    }
}

/// The unified control callback (switches/popups/sliders/color well): identify the field and
/// apply its value.
pub(crate) extern "C" fn on_control_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("on_control_changed", || {
        unsafe {
            let ctrl = sender as *mut AnyObject;
            // Refresh the slider value labels first (same as the old callbacks).
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    if ctrl == u.line_count {
                        let val: isize = msg_send![ctrl, integerValue];
                        set_field(u.line_count_value_label, val);
                    } else if ctrl == u.card_text_size || ctrl == u.status_bar_text_size {
                        let val: isize = msg_send![ctrl, integerValue];
                        if ctrl == u.card_text_size {
                            set_field(u.card_text_size_value_label, val);
                        } else {
                            // Stop index -> point size: the readout shows "0" (hidden) or "13".."20".
                            set_field(
                                u.status_bar_text_size_value_label,
                                super::status_bar_size_for_index(val as i64),
                            );
                        }
                    } else if ctrl == u.clipboard_auto_expire_days {
                        let val: isize = msg_send![ctrl, integerValue];
                        set_field(u.clipboard_auto_expire_days_value_label, val);
                    } else if ctrl == u.clipboard_image_rows {
                        let val: isize = msg_send![ctrl, integerValue];
                        set_field(u.clipboard_image_rows_value_label, val);
                    } else if ctrl == u.pointer_accel_slider {
                        // Pointer acceleration / tracking speed: the read-only value follows the drag
                        // in real time (2 decimals).
                        let val: f64 = msg_send![ctrl, doubleValue];
                        set_field(
                            u.pointer_accel_value_label,
                            pointer_accel_display(pointer_accel_from_slider(val)),
                        );
                    } else if let Some((minimum, maximum, label)) = smooth_slider_spec(u, ctrl) {
                        let val: f64 = msg_send![ctrl, doubleValue];
                        set_field(label, smooth_slider_display(val, minimum, maximum));
                    }
                }
            });
            let Some(field) = control_field_of(ctrl) else {
                log_debug!("[settings] control change from an unknown sender ignored");
                return;
            };
            // The geometry smoke exercises the real callback-to-reflow path without changing config
            // or applying device side effects.
            if !crate::dev_flags::present("smoke-settings-collapsible-row") {
                apply_control_field(field);
            }
            if matches!(
                field,
                ControlField::ClipboardDeleteAfterPaste
                    | ControlField::ClipboardClearSystemPasteboardAfterPaste
            ) {
                with_settings_ui(|ui| {
                    if let Some(u) = ui.as_ref() {
                        update_clipboard_controls_enabled(u);
                    }
                });
            }
            refresh_dependent_control_visibility(field);
            if sidebar::settings_search_is_active() {
                sidebar::schedule_search_refresh();
            }
        }
    });
}

/// Refresh conditional rows and their document geometry after a setting changes.
unsafe fn refresh_dependent_control_visibility(field: ControlField) {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return;
        };
        match field {
            ControlField::ClipboardDeleteAfterPaste => {
                // The Clipboard page's layout owner shows/hides the row and settles the document
                // height in one walk, so no call-site refit is involved.
                update_clipboard_delete_dependent_visibility(ui);
            }
            ControlField::DisablePointerAccel => {
                // The Mouse page's layout owner shows/hides the row and settles the page.
                update_pointer_accel_visibility(ui);
            }
            ControlField::ScrollMode => {
                update_mode_dependent_visibility(ui);
            }
            ControlField::ThumbnailsEnabled => {
                // The App Switcher page's layout owner shows/hides the pair and settles the page.
                update_display_mode_dependent_visibility(ui);
            }
            ControlField::FinderMenuEnabled => {
                // The page's options are meaningless while the menu is off, like every other
                // master switch on this page family.
                update_finder_menu_controls_enabled(ui);
            }
            _ => {}
        }
    });
}

/// The two `clipboard.picker_placement` values, in popup order.
const PICKER_PLACEMENT_VALUES: [&str; 2] = ["cursor", "center"];

/// Pure: the popup index a stored placement selects.
///
/// An unrecognised stored value (a hand-edited file `validate` would fall back on) shows the field's
/// own default, so the popup is never blank and never claims a mode the picker is not using.
pub(super) fn picker_placement_index(value: &str) -> isize {
    PICKER_PLACEMENT_VALUES
        .iter()
        .position(|candidate| *candidate == value)
        .map_or_else(
            || picker_placement_index(&crate::config::ClipboardSection::default().picker_placement),
            |index| index as isize,
        )
}

/// Pure: the placement a popup index stores. Out of range falls back to the default, which is what
/// the control shows then, so saving what is shown cannot invent a mode.
pub(super) fn picker_placement_for_popup_index(index: isize) -> &'static str {
    PICKER_PLACEMENT_VALUES
        .get(index as usize)
        .copied()
        .unwrap_or(PICKER_PLACEMENT_VALUES[0])
}

/// Read the control value into the in-memory CONFIG, schedule the debounced persist, and run
/// the field's immediate side effects.
fn apply_control_field(field: ControlField) {
    // Mouse-page per-device fields go through the profile channel (the selected device's profile).
    match field {
        ControlField::ReverseScroll
        | ControlField::ScrollMode
        | ControlField::LineCount
        | ControlField::DisablePointerAccel
        | ControlField::PointerAcceleration
        | ControlField::SmoothScrollingPreset
        | ControlField::SmoothScrollingResponse
        | ControlField::SmoothScrollingSpeed
        | ControlField::SmoothScrollingAcceleration
        | ControlField::SmoothScrollingInertia
        | ControlField::MappingEnabled => {
            unsafe { apply_mouse_profile_field(field) };
            return;
        }
        _ => {}
    }
    let old_cfg = CONFIG.read().unwrap().clone();
    let mut cfg = old_cfg.clone();
    with_settings_ui(|ui| {
        let Some(u) = ui.as_ref() else {
            return;
        };
        unsafe {
            match field {
                ControlField::Theme => {
                    let idx: isize = msg_send![u.theme, indexOfSelectedItem];
                    cfg.appearance.theme = match idx {
                        0 => "dark",
                        1 => "light",
                        _ => "auto",
                    }
                    .into();
                }
                ControlField::PanelMaterial => {
                    let idx: isize = msg_send![u.panel_material, indexOfSelectedItem];
                    cfg.appearance.panel_material =
                        crate::config::panel_material_value_at(idx.max(0) as usize).to_string();
                }
                ControlField::GlassStyle => {
                    let idx: isize = msg_send![u.glass_style, indexOfSelectedItem];
                    cfg.appearance.glass_style = match idx {
                        1 => "clear",
                        _ => "regular",
                    }
                    .into();
                }
                ControlField::Locale => {
                    let idx: isize = msg_send![u.locale, indexOfSelectedItem];
                    cfg.i18n.locale = LOCALE_VALUES
                        .get(idx as usize)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "auto".into());
                }
                ControlField::LogLevel => {
                    let idx: isize = msg_send![u.log_level, indexOfSelectedItem];
                    cfg.logging.level = match idx {
                        0 => "debug",
                        _ => "info",
                    }
                    .into();
                }
                ControlField::LaunchAtLogin => {
                    let state: isize = msg_send![u.launch_at_login, state];
                    cfg.startup.launch_at_login = state == 1;
                }
                ControlField::HideMenuBarIcon => {
                    let state: isize = msg_send![u.hide_menu_bar_icon, state];
                    cfg.startup.hide_menu_bar_icon = state == 1;
                }
                ControlField::WindowsEnabled => {
                    let state: isize = msg_send![u.windows_enabled, state];
                    cfg.windows.enabled = state == 1;
                }
                ControlField::ShowMinimized => {
                    let state: isize = msg_send![u.show_minimized, state];
                    cfg.windows.show_minimized = state == 1;
                }
                ControlField::ShowHiddenAppWindows => {
                    let state: isize = msg_send![u.show_hidden_app_windows, state];
                    cfg.windows.show_hidden_app_windows = state == 1;
                }
                ControlField::ShowOtherDesktops => {
                    let state: isize = msg_send![u.show_other_desktops, state];
                    cfg.windows.show_other_desktops = state == 1;
                }
                ControlField::ThumbnailsEnabled => {
                    let idx: isize = msg_send![u.thumbnails_enabled, indexOfSelectedItem];
                    cfg.layout.thumbnails_enabled = idx == 1;
                }
                ControlField::ThumbnailSize => {
                    let idx: isize = msg_send![u.thumbnail_size, indexOfSelectedItem];
                    cfg.layout.thumbnail_size =
                        crate::config::thumbnail_size_value_at(idx.max(0) as usize).to_string();
                }
                ControlField::FocusedThumbnailPrewarm => {
                    let state: isize = msg_send![u.focused_thumbnail_prewarm, state];
                    cfg.layout.focused_thumbnail_prewarm = state == 1;
                }
                ControlField::ShowAppNameInCards => {
                    let state: isize = msg_send![u.show_app_name_in_cards, state];
                    cfg.layout.show_app_name_in_cards = state == 1;
                }
                ControlField::ShowFooterTitle => {
                    let state: isize = msg_send![u.show_footer_title, state];
                    cfg.layout.show_footer_title = state == 1;
                }
                ControlField::CardTextSize => {
                    let val: isize = msg_send![u.card_text_size, integerValue];
                    cfg.layout.card_text_size = val as f64;
                }
                ControlField::StatusBarTextSize => {
                    // The slider reports a stop index (0 = hidden, 1..=8 = 13..=20pt); the config
                    // stores the point size.
                    let val: isize = msg_send![u.status_bar_text_size, integerValue];
                    cfg.fonts.status_bar_size = super::status_bar_size_for_index(val as i64) as f64;
                }
                ControlField::OverlayPosition => {
                    let idx: isize = msg_send![u.overlay_position, indexOfSelectedItem];
                    cfg.windows.overlay_position = match idx {
                        1 => "main",
                        _ => "active_window",
                    }
                    .into();
                }
                ControlField::ActivationMode => {
                    let idx: isize = msg_send![u.activation_mode, indexOfSelectedItem];
                    cfg.windows.activation_mode = match idx {
                        1 => "click",
                        _ => "hover",
                    }
                    .into();
                }
                ControlField::PanelCloseEnabled => {
                    let state: isize = msg_send![u.panel_close_enabled, state];
                    cfg.windows.panel_close_enabled = state == 1;
                }
                ControlField::PanelMinimizeEnabled => {
                    let state: isize = msg_send![u.panel_minimize_enabled, state];
                    cfg.windows.panel_minimize_enabled = state == 1;
                }
                ControlField::PanelHideEnabled => {
                    let state: isize = msg_send![u.panel_hide_enabled, state];
                    cfg.windows.panel_hide_enabled = state == 1;
                }
                ControlField::PanelQuitEnabled => {
                    let state: isize = msg_send![u.panel_quit_enabled, state];
                    cfg.windows.panel_quit_enabled = state == 1;
                }
                ControlField::CornerRadius => {
                    // Corner radius is a numeric text field on the notification path; it
                    // should never arrive via target/action.
                    log_debug!("[settings] corner radius via unexpected action path ignored");
                }
                ControlField::Modifier => {
                    let idx: isize = msg_send![u.modifier, indexOfSelectedItem];
                    cfg.keyboard.modifier =
                        crate::config::switcher_modifier_value_at(idx.max(0) as usize).into();
                }
                ControlField::KeystrokeDisplayEnabled => {
                    let state: isize = msg_send![u.keystroke_display_enabled, state];
                    cfg.keystroke_display.enabled = state == 1;
                }
                ControlField::KeystrokeDisplayMode => {
                    let idx: isize = msg_send![u.keystroke_display_mode, indexOfSelectedItem];
                    cfg.keystroke_display.mode = match idx {
                        1 => "shortcuts",
                        2 => "commands",
                        _ => "all",
                    }
                    .into();
                }
                ControlField::KeystrokeDisplayTapLevel => {
                    let idx: isize = msg_send![u.keystroke_display_tap_level, indexOfSelectedItem];
                    cfg.keystroke_display.tap_level =
                        if idx == 1 { "hid" } else { "session" }.into();
                }
                ControlField::KeystrokeDisplayPosition => {
                    let index: isize = msg_send![u.keystroke_display_position, indexOfSelectedItem];
                    cfg.keystroke_display.display_position =
                        super::KEYSTROKE_DISPLAY_POSITION_VALUES
                            .get(index.max(0) as usize)
                            .copied()
                            .unwrap_or("caret")
                            .into();
                }
                ControlField::KeystrokeDisplayInitialPosition => {
                    let index: isize =
                        msg_send![u.keystroke_display_initial_position, indexOfSelectedItem];
                    cfg.keystroke_display.initial_position =
                        super::KEYSTROKE_DISPLAY_INITIAL_POSITION_VALUES
                            .get(index.max(0) as usize)
                            .copied()
                            .unwrap_or("bottom")
                            .into();
                    // A remembered drag outranks the edge, so clear it: picking a position in
                    // Settings is a request to see the panel there now.
                    cfg.keystroke_display.position = None;
                }
                ControlField::KeystrokeDisplayMaxExtent => {
                    let raw: isize = msg_send![u.keystroke_display_max_extent, integerValue];
                    let percent =
                        crate::config::keystroke_display_max_extent_percent_snap(raw.max(0) as u32);
                    cfg.keystroke_display.max_extent_percent = percent;
                    // Snap the control itself, so a stale click cannot leave the knob between the
                    // steps the readout and the saved value speak.
                    let _: () = msg_send![
                        u.keystroke_display_max_extent,
                        setIntegerValue: percent as isize
                    ];
                    crate::settings::page_builder::set_max_extent_readout(
                        u.keystroke_display_max_extent_value,
                        percent,
                    );
                    // The live preview the user asked for: the panel stays on screen and resizes
                    // with the drag. It is a preview -- the config value above is the only thing
                    // that persists, and the panel it draws uses virtual keycaps.
                    if !crate::dev_flags::present("smoke-settings-collapsible-row") {
                        preview_max_extent(percent);
                    }
                }
                ControlField::MouseEnabled => {
                    let state: isize = msg_send![u.enable_mouse, state];
                    cfg.mouse.enabled = state == 1;
                }
                ControlField::ReverseScroll
                | ControlField::ScrollMode
                | ControlField::LineCount
                | ControlField::DisablePointerAccel
                | ControlField::PointerAcceleration
                | ControlField::SmoothScrollingPreset
                | ControlField::SmoothScrollingResponse
                | ControlField::SmoothScrollingSpeed
                | ControlField::SmoothScrollingAcceleration
                | ControlField::SmoothScrollingInertia
                | ControlField::MappingEnabled => {
                    // These fields are routed to the profile channel at the top.
                    log_debug!("[settings] mouse field via unexpected action path ignored");
                }
                ControlField::ClipboardEnabled => {
                    let state: isize = msg_send![u.clipboard_enabled, state];
                    cfg.clipboard.enabled = state == 1;
                }
                ControlField::ClipboardPaused => {
                    let state: isize = msg_send![u.clipboard_paused, state];
                    cfg.clipboard.paused = state == 1;
                }
                ControlField::ClipboardClearOnQuit => {
                    let state: isize = msg_send![u.clipboard_clear_on_quit, state];
                    cfg.clipboard.clear_on_quit = state == 1;
                }
                ControlField::ClipboardSeparateClearAll => {
                    let state: isize = msg_send![u.clipboard_separate_clear_all, state];
                    cfg.clipboard.separate_clear_all = state == 1;
                }
                ControlField::ClipboardShowSourceApp => {
                    let state: isize = msg_send![u.clipboard_show_source_app, state];
                    cfg.clipboard.show_source_app = state == 1;
                }
                ControlField::ClipboardMoveUsedToTop => {
                    let state: isize = msg_send![u.clipboard_move_used_to_top, state];
                    cfg.clipboard.move_used_to_top = state == 1;
                }
                ControlField::ClipboardDeleteAfterPaste => {
                    let state: isize = msg_send![u.clipboard_delete_after_paste, state];
                    cfg.clipboard.delete_after_paste = state == 1;
                }
                ControlField::ClipboardClearSystemPasteboardAfterPaste => {
                    let state: isize =
                        msg_send![u.clipboard_clear_system_pasteboard_after_paste, state];
                    cfg.clipboard.clear_system_pasteboard_after_paste = state == 1;
                }
                ControlField::ClipboardAutoExpireDays => {
                    let value: isize = msg_send![u.clipboard_auto_expire_days, integerValue];
                    cfg.clipboard.auto_expire_days = value.clamp(
                        CLIPBOARD_AUTO_EXPIRE_MIN as isize,
                        CLIPBOARD_AUTO_EXPIRE_MAX as isize,
                    ) as u32;
                }
                ControlField::ClipboardImageRows => {
                    let value: isize = msg_send![u.clipboard_image_rows, integerValue];
                    cfg.clipboard.image_rows = (value as u32).clamp(1, 3);
                }
                ControlField::ClipboardMaxEntries => {
                    // Numeric text fields ride the NSControlText notification path.
                    log_debug!("[settings] numeric field via unexpected action path ignored");
                }
                ControlField::ClipboardPinFollow => {
                    let idx: isize = msg_send![u.clipboard_pin_follow, indexOfSelectedItem];
                    cfg.clipboard.pin_follow_selection = idx != 1;
                }
                ControlField::ClipboardPickerPlacement => {
                    let idx: isize = msg_send![u.clipboard_picker_placement, indexOfSelectedItem];
                    cfg.clipboard.picker_placement =
                        picker_placement_for_popup_index(idx).to_string();
                }
                ControlField::KeyBindingsEnabled => {
                    let state: isize = msg_send![u.key_bindings_enabled, state];
                    cfg.key_bindings.enabled = state == 1;
                }
                ControlField::UpdateAutoCheck => {
                    let state: isize = msg_send![u.update_auto_check, state];
                    cfg.updates.automatically_check = state == 1;
                }
                ControlField::UpdateAutoDownload => {
                    let state: isize = msg_send![u.update_auto_download, state];
                    cfg.updates.automatically_download = state == 1;
                }
                ControlField::FinderMenuEnabled => {
                    let state: isize = msg_send![u.finder_menu_enabled, state];
                    cfg.finder_menu.enabled = state == 1;
                }
                ControlField::FinderMenuLayout => {
                    let idx: isize = msg_send![u.finder_menu_layout, indexOfSelectedItem];
                    cfg.finder_menu.layout = crate::config::FINDER_MENU_LAYOUTS
                        .get(idx.max(0) as usize)
                        .copied()
                        .unwrap_or(crate::config::FINDER_MENU_DEFAULT_LAYOUT)
                        .to_string();
                }
                ControlField::FinderMenuNewFileType(_) => {
                    let states: Vec<bool> = u
                        .finder_menu_new_file_switches
                        .iter()
                        .map(|control| finder_menu_switch_on(*control))
                        .collect();
                    cfg.finder_menu.new_file_types = checked_ids(
                        crate::config::FINDER_MENU_FILE_TYPES.iter().copied(),
                        &states,
                    );
                }
                ControlField::FinderMenuTerminal(_) => {
                    let states: Vec<bool> = u
                        .finder_menu_terminal_switches
                        .iter()
                        .map(|control| finder_menu_switch_on(*control))
                        .collect();
                    cfg.finder_menu.terminals = checked_ids(
                        crate::finder_menu::terminals()
                            .iter()
                            .map(|spec| spec.bundle_id),
                        &states,
                    );
                }
                ControlField::FinderMenuCopyPath => {
                    let state: isize = msg_send![u.finder_menu_copy_path, state];
                    cfg.finder_menu.copy_path = state == 1;
                }
            }
        }
    });
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    schedule_config_persist();
    apply_config_change(&old_cfg, &cfg, ConfigChangeSource::Settings);
}

/// Mouse-page per-device fields: read the control → write the selected device's profile
/// (created if absent) → persist + side effects.
pub(super) unsafe fn apply_mouse_profile_field(field: ControlField) {
    let update = with_settings_ui(|ui| {
        let u = ui.as_mut()?;
        let old_cfg = CONFIG.read().unwrap().clone();
        let mut cfg = old_cfg.clone();
        match field {
            ControlField::ReverseScroll => {
                let state: isize = msg_send![u.reverse_scroll, state];
                write_selected_profile(&mut cfg, move |p| p.reverse_scroll = Some(state == 1));
            }
            ControlField::ScrollMode => {
                let idx: isize = msg_send![u.scroll_mode, indexOfSelectedItem];
                let mode = SCROLL_MODE_VALUES
                    .get(idx as usize)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "default".into());
                // Unlike LinearMouse, mode selection retains both line count and smooth tuning;
                // only the active path changes so switching back restores each side's values.
                write_selected_profile(&mut cfg, move |p| {
                    p.scroll_mode = Some(mode);
                });
            }
            ControlField::LineCount => {
                let lc: isize = msg_send![u.line_count, integerValue];
                write_selected_profile(&mut cfg, move |p| {
                    p.line_count = Some(lc.clamp(1, 10) as u32)
                });
            }
            ControlField::DisablePointerAccel => {
                let state: isize = msg_send![u.disable_pointer_accel, state];
                // Change only this field: the other one (tracking speed) must survive untouched
                // rather than being overwritten by a whole-section replacement.
                write_selected_profile(&mut cfg, move |p| {
                    let mut ptr = p.pointer.take().unwrap_or_default();
                    ptr.disable_acceleration = Some(state == 1);
                    p.pointer = Some(ptr);
                });
            }
            ControlField::PointerAcceleration => {
                let raw: f64 = msg_send![u.pointer_accel_slider, doubleValue];
                // Clamped to 0..=10 with 2 decimals; a non-finite value falls back (see
                // pointer_accel_from_slider).
                let value = pointer_accel_from_slider(raw);
                write_selected_profile(&mut cfg, move |p| {
                    let mut ptr = p.pointer.take().unwrap_or_default();
                    ptr.acceleration = Some(value);
                    p.pointer = Some(ptr);
                });
            }
            ControlField::SmoothScrollingPreset => {
                let index: isize = msg_send![u.smooth_scrolling_preset, indexOfSelectedItem];
                let preset = crate::mouse::smooth::presets::SmoothPreset::ALL
                    .get(index.max(0) as usize)
                    .copied()
                    .unwrap_or(crate::mouse::smooth::presets::SmoothPreset::EaseInOut);
                let (response, speed, acceleration, inertia) = preset.default_settings();
                write_selected_profile(&mut cfg, move |p| {
                    let mut smooth = p.smooth_scrolling.take().unwrap_or_default();
                    smooth.preset = Some(preset.as_str().to_string());
                    if preset != crate::mouse::smooth::presets::SmoothPreset::Custom {
                        smooth.response = Some(response);
                        smooth.speed = Some(speed);
                        smooth.acceleration = Some(acceleration);
                        smooth.inertia = Some(inertia);
                    }
                    p.smooth_scrolling = Some(smooth);
                });
            }
            ControlField::SmoothScrollingResponse
            | ControlField::SmoothScrollingSpeed
            | ControlField::SmoothScrollingAcceleration
            | ControlField::SmoothScrollingInertia => {
                let (slider, minimum, maximum) = match field {
                    ControlField::SmoothScrollingResponse => {
                        (u.smooth_scrolling_response, 0.0, 2.0)
                    }
                    ControlField::SmoothScrollingSpeed => (u.smooth_scrolling_speed, 0.0, 8.0),
                    ControlField::SmoothScrollingAcceleration => {
                        (u.smooth_scrolling_acceleration, 0.0, 8.0)
                    }
                    ControlField::SmoothScrollingInertia => (u.smooth_scrolling_inertia, 0.0, 8.0),
                    _ => unreachable!(),
                };
                let raw: f64 = msg_send![slider, doubleValue];
                let value = smooth_slider_value(raw, minimum, maximum);
                write_selected_profile(&mut cfg, move |p| {
                    let mut smooth = p.smooth_scrolling.take().unwrap_or_default();
                    smooth.preset = Some("custom".into());
                    match field {
                        ControlField::SmoothScrollingResponse => smooth.response = Some(value),
                        ControlField::SmoothScrollingSpeed => smooth.speed = Some(value),
                        ControlField::SmoothScrollingAcceleration => {
                            smooth.acceleration = Some(value)
                        }
                        ControlField::SmoothScrollingInertia => smooth.inertia = Some(value),
                        _ => unreachable!(),
                    }
                    p.smooth_scrolling = Some(smooth);
                });
            }
            ControlField::MappingEnabled => {
                let state: isize = msg_send![u.mapping_enabled, state];
                write_selected_profile(&mut cfg, move |p| {
                    p.button_mappings_enabled = Some(state == 1)
                });
            }
            _ => {}
        }
        Some((old_cfg, cfg))
    });
    let Some((old_cfg, cfg)) = update else {
        return;
    };
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    if !crate::dev_flags::present("smoke-settings-mouse-profile-callback") {
        schedule_config_persist();
    }
    // Applying the config refreshes the visible controls synchronously, so do it after the
    // SettingsUi RefCell borrow has been released.
    apply_config_change(&old_cfg, &cfg, ConfigChangeSource::Settings);
}

/// Hand the selected device's profile to the callback (creating one when absent).
fn write_selected_profile(cfg: &mut Config, f: impl FnOnce(&mut MouseProfile)) {
    // Creating a missing profile is handled in one place (including the virtual-pointer case).
    let idx = super::selected_device_profile_index(cfg);
    f(&mut cfg.mouse.profiles[idx]);
}

/// While typing in a numeric text field (NSControlTextDidChange): apply only when the value
/// is valid; invalid input leaves the in-memory config untouched and persistence goes
/// through the debounce. Transient invalid states are allowed while typing.
pub(crate) extern "C" fn on_control_text_did_change(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut c_void,
) {
    callback_guard::void("on_control_text_did_change", || unsafe {
        let obj: *mut AnyObject = msg_send![notification as *mut AnyObject, object];
        let field = match text_field_of(obj) {
            Some(f) => f,
            None => return,
        };
        let Some((value, raw)) = parse_text_field(field) else {
            return;
        };
        let _ = raw;
        EDITED_TEXT_FIELDS.fetch_or(text_field_bit(field), Ordering::SeqCst);
        write_text_field_config(field, value, false);
        schedule_config_persist();
        log_debug!(
            "[settings] numeric field {:?} applied (debounced persist)",
            field
        );
    });
}

/// On blur / Enter (NSControlTextDidEndEditing): commit immediately and persist now; when
/// still invalid, restore the displayed value from the in-memory config.
pub(crate) extern "C" fn on_control_text_did_end_editing(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut c_void,
) {
    callback_guard::void("on_control_text_did_end_editing", || {
        unsafe {
            let obj: *mut AnyObject = msg_send![notification as *mut AnyObject, object];
            let field = match text_field_of(obj) {
                Some(f) => f,
                None => return,
            };
            match parse_text_field(field) {
                Some((value, _)) => {
                    // Only a field the user actually typed into commits here. An untouched field is AppKit
                    // resigning the window's automatic initial first responder (see `EDITED_TEXT_FIELDS`), and
                    // committing it would rewrite the config file on every open.
                    if !take_edited_field(&EDITED_TEXT_FIELDS, field) {
                        log_debug!(
                            "[settings] numeric field {field:?} untouched on end editing; not persisted"
                        );
                        return;
                    }
                    write_text_field_config(field, value, true);
                    persist_config_now();
                    log_debug!(
                        "[settings] numeric field {:?} committed on end editing",
                        field
                    );
                }
                None => {
                    // The edit session is over either way, so the bit does not outlive it.
                    let _ = take_edited_field(&EDITED_TEXT_FIELDS, field);
                    // Do not commit invalid values; restore the last effective config value.
                    let cfg = CONFIG.read().unwrap().clone();
                    let text = match field {
                        TextField::CornerRadius => cfg.appearance.corner_radius.to_string(),
                        TextField::ClipboardMaxEntries => cfg.clipboard.max_entries.to_string(),
                    };
                    with_settings_ui(|ui| {
                        if let Some(u) = ui.as_ref() {
                            let ctrl = match field {
                                TextField::CornerRadius => u.corner_radius,
                                TextField::ClipboardMaxEntries => u.clipboard_max_entries,
                            };
                            set_field(ctrl, text);
                        }
                    });
                }
            }
        }
    });
}

/// Numeric text-field ids (separate from ControlField: they use notification callbacks, not
/// target/action).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextField {
    CornerRadius,
    ClipboardMaxEntries,
}

unsafe fn text_field_of(obj: *mut AnyObject) -> Option<TextField> {
    with_settings_ui(|ui| {
        let u = ui.as_ref()?;
        let ptr = obj as usize;
        if ptr == u.corner_radius as usize {
            Some(TextField::CornerRadius)
        } else if ptr == u.clipboard_max_entries as usize {
            Some(TextField::ClipboardMaxEntries)
        } else {
            None
        }
    })
}

/// Parse a numeric text field: returns (normalized value, raw text); None when invalid.
unsafe fn parse_text_field(field: TextField) -> Option<(TextFieldValue, String)> {
    with_settings_ui(|ui| {
        let u = ui.as_ref()?;
        let (ctrl, bounds): (*mut AnyObject, (f64, f64)) = match field {
            TextField::CornerRadius => (u.corner_radius, (0.0, 500.0)),
            TextField::ClipboardMaxEntries => (u.clipboard_max_entries, (1.0, 100.0)),
        };
        let raw = nsstring_to_rust(msg_send![ctrl, stringValue]);
        let value = match field {
            TextField::CornerRadius => match parse_f64(&raw) {
                Ok(v) if v.is_finite() && v >= bounds.0 && v <= bounds.1 => TextFieldValue::F64(v),
                _ => return None,
            },
            TextField::ClipboardMaxEntries => match parse_usize(&raw) {
                Ok(v) if (bounds.0 as usize..=bounds.1 as usize).contains(&v) => {
                    TextFieldValue::U32(v as u32)
                }
                _ => return None,
            },
        };
        Some((value, raw))
    })
}

#[derive(Clone, Copy)]
enum TextFieldValue {
    F64(f64),
    U32(u32),
}

/// One bit per numeric field, set by a real keystroke and consumed by the commit on end editing.
///
/// The distinction matters because `on_control_text_did_change` already wrote a valid keystroke into
/// `CONFIG`, so "did this field change?" cannot tell the two cases apart. What can is whether the user ever
/// typed into the field at all: the prewarmed settings window takes AppKit's automatic initial first
/// responder, so the first real open resigns a field nobody touched, and committing that would rewrite
/// `config.toml` on every open.
static EDITED_TEXT_FIELDS: AtomicU32 = AtomicU32::new(0);

fn text_field_bit(field: TextField) -> u32 {
    match field {
        TextField::CornerRadius => 1 << 0,
        TextField::ClipboardMaxEntries => 1 << 1,
    }
}

/// Consume the field's "the user edited this" bit. Takes the mask so the contract is testable without the
/// global.
fn take_edited_field(mask: &AtomicU32, field: TextField) -> bool {
    mask.fetch_and(!text_field_bit(field), Ordering::SeqCst) & text_field_bit(field) != 0
}

/// Every numeric control is new after a content rebuild: no field carries an edit any more.
pub(super) fn note_text_fields_rebuilt() {
    EDITED_TEXT_FIELDS.store(0, Ordering::SeqCst);
}

/// Write a valid numeric value into the in-memory CONFIG.
fn write_text_field_config(field: TextField, value: TextFieldValue, apply_runtime: bool) {
    let old_cfg = CONFIG.read().unwrap().clone();
    let mut cfg = old_cfg.clone();
    match (field, value) {
        (TextField::CornerRadius, TextFieldValue::F64(v)) => cfg.appearance.corner_radius = v,
        (TextField::ClipboardMaxEntries, TextFieldValue::U32(v)) => cfg.clipboard.max_entries = v,
        _ => {}
    }
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    if apply_runtime {
        apply_config_change(&old_cfg, &cfg, ConfigChangeSource::Settings);
    }
}

/// Freeze or unfreeze all mouse controls below the enable_mouse switch based on its state.
/// When disabled, controls are greyed out and non-interactive (AppKit handles greying), preventing
/// users from editing config that won't take effect.
pub(super) unsafe fn update_mouse_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.enable_mouse, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_mouse_disabled");
    let device_available = !DEVICE_POPUP_KEYS.lock().unwrap().is_empty();
    // Keep the device popup disabled when no device is connected; the remaining controls follow
    // the master switch.
    if on {
        SettingsRow::set_enabled(ui.device_indicator, device_available);
    } else {
        SettingsRow::set_enabled_with_tooltip(ui.device_indicator, false, &tooltip);
    }
    for &ctrl in &[
        ui.scroll_mode,
        ui.reverse_scroll,
        ui.disable_pointer_accel,
        ui.pointer_accel_slider,
        ui.pointer_accel_value_label,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
    if on {
        SettingsRow::set_enabled(ui.line_count, selected_scroll_mode(ui) == ScrollMode::Line);
    } else {
        SettingsRow::set_enabled_with_tooltip(ui.line_count, false, &tooltip);
    }
    update_smooth_controls_enabled(ui);
    // The virtual-pointer profile (a software KVM's injected mouse) has no HID service client, so
    // acceleration / tracking speed has nowhere to be written (pointer::apply finds no device by
    // VID/PID). Those two controls can never take effect in that profile, so they get a "not
    // applicable" hint and are greyed out instead of inviting an edit that does nothing. Scrolling,
    // line mode and button mappings all work in that profile and are left alone.
    if on && current_selected_device() == Some(crate::mouse::device::VIRTUAL_DEVICE_KEY) {
        let virtual_tooltip = t("settings.tooltip_pointer_virtual_unsupported");
        for &ctrl in &[
            ui.disable_pointer_accel,
            ui.pointer_accel_slider,
            ui.pointer_accel_value_label,
        ] {
            SettingsRow::set_enabled_with_tooltip(ctrl, false, &virtual_tooltip);
        }
    }
    update_mapping_controls_enabled(ui);
}

unsafe fn update_smooth_controls_enabled(ui: &SettingsUi) {
    let mouse_state: isize = msg_send![ui.enable_mouse, state];
    let enabled = mouse_state == 1 && selected_scroll_mode(ui) == ScrollMode::Smooth;
    let controls = [
        ui.smooth_scrolling_preset,
        ui.smooth_scrolling_response,
        ui.smooth_scrolling_response_value,
        ui.smooth_scrolling_speed,
        ui.smooth_scrolling_speed_value,
        ui.smooth_scrolling_acceleration,
        ui.smooth_scrolling_acceleration_value,
        ui.smooth_scrolling_inertia,
        ui.smooth_scrolling_inertia_value,
    ];
    if mouse_state == 1 {
        for control in controls {
            SettingsRow::set_enabled(control, enabled);
        }
    } else {
        let tooltip = t("settings.tooltip_mouse_disabled");
        for control in controls {
            SettingsRow::set_enabled_with_tooltip(control, false, &tooltip);
        }
    }
}

unsafe fn selected_scroll_mode(ui: &SettingsUi) -> ScrollMode {
    let idx: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
    SCROLL_MODE_VALUES
        .get(idx as usize)
        .map(|value| ScrollMode::from_str(value))
        .unwrap_or(ScrollMode::Default)
}

/// Freeze the window and keyboard options below the app-switcher master switch.
pub(super) unsafe fn update_windows_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.windows_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_windows_disabled");
    for &ctrl in &[
        ui.show_minimized,
        ui.show_hidden_app_windows,
        ui.show_other_desktops,
        ui.thumbnails_enabled,
        ui.focused_thumbnail_prewarm,
        ui.show_app_name_in_cards,
        ui.thumbnail_size,
        ui.card_text_size,
        ui.card_text_size_value_label,
        ui.status_bar_text_size,
        ui.status_bar_text_size_value_label,
        ui.show_footer_title,
        ui.overlay_position,
        ui.activation_mode,
        ui.panel_close_enabled,
        ui.panel_minimize_enabled,
        ui.panel_hide_enabled,
        ui.panel_quit_enabled,
        ui.corner_radius,
        ui.modifier,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// Freeze the clipboard-history options below the clipboard master switch.
/// Re-read the pause switch from the live configuration. Used by the menu item and by the apply
/// pass when a resume could not establish its baseline and the intent was forced back to paused.
pub(super) fn refresh_clipboard_pause_switch_from_config() {
    let paused = CONFIG.read().unwrap().clipboard.paused;
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                if !u.clipboard_paused.is_null() {
                    let _: () = msg_send![u.clipboard_paused, setState: if paused { 1isize } else { 0isize }];
                }
            }
        });
    }
}

/// Apply the pause switch, then mirror the EFFECTIVE state back into it: a resume that could not
/// read the pasteboard keeps recording off, and the switch must not claim otherwise.
pub(crate) extern "C" fn handle_pause_recording_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_pause_recording_toggle", || {
        apply_control_field(ControlField::ClipboardPaused);
        refresh_clipboard_pause_switch_from_config();
        crate::menu::refresh_clipboard_pause_item();
    });
}

/// Mutate the excluded-applications list the way every other settings control commits: write the
/// in-memory config, persist, run the apply pass, then rebuild the page (the card is laid out from
/// the list). Duplicate and empty identifiers are collapsed so the list cannot grow entries that
/// can never match.
fn mutate_excluded_apps(mutate: impl FnOnce(&mut Vec<crate::config::ExcludedApp>)) {
    let old_cfg = match CONFIG.read() {
        Ok(cfg) => cfg.clone(),
        Err(_) => return,
    };
    let mut new_cfg = old_cfg.clone();
    mutate(&mut new_cfg.clipboard.excluded_apps);
    let mut seen: Vec<String> = Vec::new();
    new_cfg.clipboard.excluded_apps.retain(|entry| {
        let id = entry.bundle_id.trim().to_ascii_lowercase();
        if id.is_empty() || seen.contains(&id) {
            return false;
        }
        seen.push(id);
        true
    });
    if new_cfg == old_cfg {
        return;
    }
    if let Ok(mut current) = CONFIG.write() {
        *current = new_cfg.clone();
    }
    persist_config_now();
    apply_config_change(&old_cfg, &new_cfg, ConfigChangeSource::Settings);
    // Deferred: rebuilding inside the button's own action would release the button AppKit is
    // currently dispatching to. The page keeps its scroll position, so editing a list of rows does
    // not throw the user back to the top of the page.
    crate::settings::rebuild_settings_content_deferred_keeping_scroll(SettingsPageId::Clipboard);
}

/// Add the running app selected in the add popup. Index 0 is the placeholder, so the candidate is
/// (index - 1) in the list captured when the page was built.
pub(crate) extern "C" fn handle_clipboard_excluded_add(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    callback_guard::void("handle_clipboard_excluded_add", || {
        if sender.is_null() {
            return;
        }
        let index: isize = unsafe { msg_send![sender as *mut AnyObject, indexOfSelectedItem] };
        if index <= 0 {
            return;
        }
        // The popup's trailing entry is "choose from disk": the same intent, for an app that is not
        // running. One add control, two sources.
        let choose_from_disk = with_settings_ui(|ui| {
            ui.as_ref()
                .map(|u| index as usize == u.clipboard_excluded_candidates.len() + 1)
                .unwrap_or(false)
        });
        if choose_from_disk {
            add_excluded_app_from_disk();
            return;
        }
        let candidate = with_settings_ui(|ui| {
            ui.as_ref().and_then(|u| {
                u.clipboard_excluded_candidates
                    .get((index - 1) as usize)
                    .cloned()
            })
        });
        let Some((bundle_id, name)) = candidate else {
            return;
        };
        mutate_excluded_apps(|list| {
            if !list
                .iter()
                .any(|e| e.bundle_id.eq_ignore_ascii_case(&bundle_id))
            {
                list.push(crate::config::ExcludedApp::new(bundle_id, name));
            }
        });
    });
}

/// Add an application chosen from disk, so an app that is not currently running can still be
/// excluded (the popup only knows running applications).
/// Pick an application bundle from disk and add it to the exclusion list. Reached from the add
/// popup's trailing entry, so an app that is not running can still be excluded.
fn add_excluded_app_from_disk() {
    let Some(path) = (unsafe { crate::ffi::run_open_panel_for_app() }) else {
        return;
    };
    let Some((bundle_id, name)) = (unsafe { crate::app_identity::bundle_identity_at_path(&path) })
    else {
        // Not an application bundle, or one without an identifier: nothing can be matched against
        // it, so it is refused rather than stored as an inert entry.
        crate::log_info!("[settings] excluded app ignored: not an application bundle");
        return;
    };
    mutate_excluded_apps(|list| {
        if !list
            .iter()
            .any(|e| e.bundle_id.eq_ignore_ascii_case(&bundle_id))
        {
            list.push(crate::config::ExcludedApp::new(bundle_id, name));
        }
    });
}

/// Remove the row whose remove button was pressed. The sender POINTER identifies the entry: a
/// button tag is reserved for its hover/normal palette and must never carry an action id.
pub(crate) extern "C" fn handle_clipboard_excluded_remove(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    callback_guard::void("handle_clipboard_excluded_remove", || {
        if sender.is_null() {
            return;
        }
        let index = with_settings_ui(|ui| {
            ui.as_ref().and_then(|u| {
                u.clipboard_excluded_remove
                    .iter()
                    .position(|pointer| *pointer == sender as *mut AnyObject)
            })
        });
        let Some(index) = index else {
            return;
        };
        mutate_excluded_apps(|list| {
            if index < list.len() {
                list.remove(index);
            }
        });
    });
}

/// Restore the built-in exclusion list.
pub(crate) extern "C" fn handle_clipboard_excluded_restore(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_clipboard_excluded_restore", || {
        mutate_excluded_apps(|list| *list = crate::config::default_excluded_apps());
    });
}

/// Deferred page rebuild requested by settings::rebuild_settings_content_deferred.
pub(crate) extern "C" fn handle_rebuild_settings_content(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_rebuild_settings_content", || {
        let pending = crate::settings::take_rebuild_request();
        match pending {
            Some((page, offset)) => unsafe {
                super::window::rebuild_settings_content_now_keeping_scroll(page, Some(offset));
            },
            None => unsafe { super::window::rebuild_settings_content_now() },
        }
    });
}

pub(super) unsafe fn update_clipboard_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.clipboard_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_clipboard_disabled");
    // "Clear the matching system-pasteboard entry" is in this list too: only the master switch
    // greys it out (whether it shows at all is decided by "delete entry after paste", see
    // update_clipboard_delete_dependent_visibility), so it no longer stacks a second condition.
    for &ctrl in &[
        ui.clipboard_paused,
        ui.clipboard_shortcut,
        ui.clipboard_pin_follow,
        ui.clipboard_picker_placement,
        ui.clipboard_clear_on_quit,
        ui.clipboard_separate_clear_all,
        ui.clipboard_show_source_app,
        ui.clipboard_move_used_to_top,
        ui.clipboard_delete_after_paste,
        ui.clipboard_clear_system_pasteboard_after_paste,
        ui.clipboard_max_entries,
        ui.clipboard_auto_expire_days,
        ui.clipboard_image_rows,
        ui.clipboard_auto_expire_days_value_label,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// The slider value's display format (2 decimals, matching the read-only value label).
pub(super) fn pointer_accel_display(value: f64) -> String {
    format!("{value:.2}")
}

/// Slider raw value -> config value: clamped to 0..=10 with 2 decimals; non-finite input falls
/// back.
pub(super) fn pointer_accel_from_slider(value: f64) -> f64 {
    if !value.is_finite() {
        return crate::mouse::pointer::FALLBACK_ACCELERATION;
    }
    let clamped = value.clamp(
        crate::config::MOUSE_ACCELERATION_MIN,
        crate::config::MOUSE_ACCELERATION_MAX,
    );
    // Keep 2 decimals (matching the read-only value label, and keeping floating-point noise out
    // of the config).
    (clamped * 100.0).round() / 100.0
}

unsafe fn smooth_slider_spec(
    ui: &SettingsUi,
    slider: *mut AnyObject,
) -> Option<(f64, f64, *mut AnyObject)> {
    if slider == ui.smooth_scrolling_response {
        Some((0.0, 2.0, ui.smooth_scrolling_response_value))
    } else if slider == ui.smooth_scrolling_speed {
        Some((0.0, 8.0, ui.smooth_scrolling_speed_value))
    } else if slider == ui.smooth_scrolling_acceleration {
        Some((0.0, 8.0, ui.smooth_scrolling_acceleration_value))
    } else if slider == ui.smooth_scrolling_inertia {
        Some((0.0, 8.0, ui.smooth_scrolling_inertia_value))
    } else {
        None
    }
}

fn smooth_slider_display(value: f64, minimum: f64, maximum: f64) -> String {
    let value = if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        minimum
    };
    format!("{:.1}", (value * 10.0).round() / 10.0)
}

fn smooth_slider_value(value: f64, minimum: f64, maximum: f64) -> f64 {
    if value.is_finite() {
        (value.clamp(minimum, maximum) * 10.0).round() / 10.0
    } else {
        minimum
    }
}

/// Refresh the conditional visibility of the tracking-speed row from the disable-acceleration
/// switch:
/// - switch on (linear tracking): the "Tracking speed" row is shown
/// - switch off: the row is hidden and the sections below move up
///
/// The value only takes effect while the switch is on: HIDPointerAcceleration is the tracking
/// speed under linear tracking and the acceleration curve's strength otherwise, a different
/// meaning (see the module comment in mouse/pointer.rs) -- so it is neither shared nor shown
/// while off.
pub(super) unsafe fn update_pointer_accel_visibility(ui: &SettingsUi) {
    let state: isize = msg_send![ui.disable_pointer_accel, state];
    ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
        .set_group_visible(RowGroup::PointerAccel, state == 1);
}

/// Refresh the conditional line and smooth controls from the selected mode.
pub(super) unsafe fn update_mode_dependent_visibility(ui: &SettingsUi) {
    let mode = selected_scroll_mode(ui);
    ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
        .set_group_visible(RowGroup::LineCount, mode == ScrollMode::Line);
    ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
        .set_group_visible(RowGroup::SmoothScrolling, mode == ScrollMode::Smooth);
    let mouse_enabled: isize = msg_send![ui.enable_mouse, state];
    if mouse_enabled == 1 {
        SettingsRow::set_enabled(ui.line_count, mode == ScrollMode::Line);
    }
    update_smooth_controls_enabled(ui);
}

/// Refresh the visibility of the thumbnail-only pair from the window display mode:
/// - icons and thumbnails (index 1): show focused prewarm and "app name on thumbnails"
/// - icons only (index 0): hide both (neither means anything without thumbnails)
unsafe fn update_display_mode_dependent_visibility(ui: &SettingsUi) {
    let idx: isize = msg_send![ui.thumbnails_enabled, indexOfSelectedItem];
    // Popup index 0 = icons only, 1 = icons and thumbnails (same as the layout.thumbnails_enabled
    // boolean).
    ui.page_canvases[SETTINGS_SWITCHER_PAGE_INDEX]
        .set_group_visible(RowGroup::ThumbnailOnly, idx == 1);
}

/// Show the "clear the matching system-pasteboard entry" row only while "delete entry after
/// paste" is on (it is that switch's child option).
fn clipboard_delete_dependent_visibility_from_config(cfg: &Config) -> bool {
    cfg.clipboard.delete_after_paste
}

unsafe fn set_clipboard_delete_dependent_visibility(ui: &SettingsUi, visible: bool) {
    ui.page_canvases[SETTINGS_CLIPBOARD_PAGE_INDEX]
        .set_group_visible(RowGroup::ClipboardDeleteChild, visible);
}

/// Refresh the settings window's top warning banner for the current storage state. The state is
/// decided by the storage load, which can land while this window is already open, so the clipboard
/// module calls this when it changes (the page build and the window's own show paths call it too).
pub(crate) unsafe fn refresh_clipboard_unavailable_notice() {
    with_settings_ui(|ui| {
        if let Some(ui) = ui.as_ref() {
            super::window::refresh_settings_warning_banner(ui);
        }
    });
}

unsafe fn update_clipboard_delete_dependent_visibility(ui: &SettingsUi) {
    let state: isize = msg_send![ui.clipboard_delete_after_paste, state];
    set_clipboard_delete_dependent_visibility(ui, state == 1);
}

/// Apply the saved clipboard setting after the controls have been populated.
pub(super) unsafe fn update_clipboard_delete_dependent_visibility_from_config(
    ui: &SettingsUi,
    cfg: &Config,
) {
    set_clipboard_delete_dependent_visibility(
        ui,
        clipboard_delete_dependent_visibility_from_config(cfg),
    );
}

/// Recompute all conditional row blocks. Idempotent (the state comes from the live frames),
/// so it is safe both before and after the window is on screen.
pub(super) unsafe fn update_conditional_rows(ui: &SettingsUi) {
    update_mode_dependent_visibility(ui);
    update_pointer_accel_visibility(ui);
    update_display_mode_dependent_visibility(ui);
    update_clipboard_delete_dependent_visibility(ui);
    update_panel_material_dependent_visibility(ui);
}

/// Show the glass-style sub-option only while Liquid Glass is the selected material. It is the only
/// structural effect a panel-material change has on the settings page; the settings window's own
/// colors follow the theme, so this is reflowed in place instead of rebuilding the content.
unsafe fn update_panel_material_dependent_visibility(ui: &SettingsUi) {
    let liquid_glass = crate::config::effective_panel_material().as_str() == "liquid-glass";
    ui.page_canvases[SETTINGS_GENERAL_PAGE_INDEX]
        .set_group_visible(RowGroup::GlassStyle, liquid_glass);
}

/// Callback when the enable_mouse switch is toggled: apply immediately, then freeze/unfreeze
/// the controls below.
pub(crate) extern "C" fn handle_enable_mouse_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_enable_mouse_toggle", || {
        apply_control_field(ControlField::MouseEnabled);
        unsafe {
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    update_mouse_controls_enabled(u);
                }
            });
        }
    });
}

/// Callback for the app-switcher master switch: apply immediately, then freeze/unfreeze the
/// window and keyboard options below.
pub(crate) extern "C" fn handle_windows_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_windows_enabled_toggle", || {
        apply_control_field(ControlField::WindowsEnabled);
        unsafe {
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    update_windows_controls_enabled(u);
                }
            });
        }
    });
}

/// Callback for the clipboard-history master switch: apply immediately, then freeze/unfreeze
/// the history options below.
pub(crate) extern "C" fn handle_clipboard_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_clipboard_enabled_toggle", || {
        apply_control_field(ControlField::ClipboardEnabled);
        unsafe {
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    update_clipboard_controls_enabled(u);
                }
            });
        }
    });
}

/// Callback for the Key Bindings master switch: apply it, then freeze or unfreeze the binding rows
/// and the add row.
pub(crate) extern "C" fn handle_key_bindings_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_key_bindings_toggle", || {
        apply_control_field(ControlField::KeyBindingsEnabled);
        unsafe {
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    super::bindings::update_binding_controls_enabled(u);
                }
            });
        }
    });
}

/// Refresh top-level service switches when they are changed from the status-bar menu.
pub(crate) fn refresh_service_controls_from_config() {
    let cfg = CONFIG.read().unwrap().clone();
    unsafe {
        with_settings_ui(|ui| {
            let Some(u) = ui.as_ref() else {
                return;
            };
            for (ctrl, value) in [
                (u.windows_enabled, cfg.windows.enabled),
                (u.enable_mouse, cfg.mouse.enabled),
                (u.clipboard_enabled, cfg.clipboard.enabled),
                (u.key_bindings_enabled, cfg.key_bindings.enabled),
                (u.keystroke_display_enabled, cfg.keystroke_display.enabled),
            ] {
                let _: () = msg_send![ctrl, setState: if value { 1isize } else { 0isize }];
            }
            update_windows_controls_enabled(u);
            update_mouse_controls_enabled(u);
            update_clipboard_controls_enabled(u);
            super::bindings::update_binding_controls_enabled(u);
        });
    }
}

/// Re-read the General appearance controls after a change that does not rebuild the window
/// (currently a panel-material change): the material and glass-style popups' selected values plus
/// the dependent row's visibility. Deliberately not `load_settings_values`: a material click must
/// not overwrite a value the user is editing elsewhere on the page. An open settings window is
/// refreshed in place; a hidden or absent one is left alone.
pub(crate) fn refresh_appearance_controls_from_config() {
    let cfg = CONFIG.read().unwrap().clone();
    unsafe {
        with_settings_ui(|ui| {
            let Some(u) = ui.as_ref() else {
                return;
            };
            let material_idx =
                crate::config::panel_material_index_of(&cfg.appearance.panel_material).unwrap_or(0)
                    as isize;
            let _: () = msg_send![u.panel_material, selectItemAtIndex: material_idx];
            let glass_idx = crate::glass::glass_style_index(&cfg.appearance.glass_style) as isize;
            let _: () = msg_send![u.glass_style, selectItemAtIndex: glass_idx];
            update_conditional_rows(u);
        });
    }
}

/// Refresh switcher and Keystroke Display controls after an external config change.
///
/// This intentionally updates the existing controls in place instead of rebuilding the settings
/// window. That preserves unsaved text edits and, like the appearance refresh path, never brings
/// a hidden settings window to the foreground.
pub(crate) fn refresh_switcher_keystroke_and_mouse_controls_from_config() {
    let cfg = CONFIG.read().unwrap().clone();
    // A config change is exactly what makes the mouse page stale, and that is true whether the window is
    // hidden, on another page, or showing the mouse page. Recording it here -- not inside the visibility
    // branch below -- is what keeps the lazy fill from showing the values a reload replaced.
    super::note_mouse_page_dirty();
    unsafe {
        with_settings_ui(|ui| {
            let Some(u) = ui.as_mut() else {
                return;
            };
            let visible: bool = msg_send![u.window, isVisible];
            if !visible {
                return;
            }

            let recording = *REC_STAGE.lock().unwrap() != RecStage::Idle
                && *REC_MODE.lock().unwrap() == RecMode::ClipboardShortcut;
            set_clipboard_shortcut_capture_state(u, &cfg.clipboard.shortcut, recording, None);

            // Not mouse-specific: the display-mode pair and the clipboard-delete rows need this on every
            // config change, whatever page is on screen.
            update_conditional_rows(u);

            let modifier_idx =
                crate::config::switcher_modifier_index_of(&cfg.keyboard.modifier) as isize;
            let _: () = msg_send![u.modifier, selectItemAtIndex: modifier_idx];

            // The in-overlay action switches follow a reload like every other control: a config
            // file edit or a defaults restore must not leave the page showing the old state.
            for (control, on) in [
                (u.panel_close_enabled, cfg.windows.panel_close_enabled),
                (u.panel_minimize_enabled, cfg.windows.panel_minimize_enabled),
                (u.panel_hide_enabled, cfg.windows.panel_hide_enabled),
                (u.panel_quit_enabled, cfg.windows.panel_quit_enabled),
                (u.show_app_name_in_cards, cfg.layout.show_app_name_in_cards),
            ] {
                let _: () = msg_send![control, setState: if on { 1isize } else { 0isize }];
            }

            let _: () = msg_send![
                u.keystroke_display_enabled,
                setState: if cfg.keystroke_display.enabled { 1isize } else { 0isize }
            ];
            let mode_idx: isize = match cfg.keystroke_display.mode.as_str() {
                "shortcuts" => 1,
                "commands" => 2,
                _ => 0,
            };
            let _: () = msg_send![u.keystroke_display_mode, selectItemAtIndex: mode_idx];
            let tap_idx: isize = if cfg.keystroke_display.tap_level == "hid" {
                1
            } else {
                0
            };
            let _: () = msg_send![u.keystroke_display_tap_level, selectItemAtIndex: tap_idx];
            let display_position_idx = super::KEYSTROKE_DISPLAY_POSITION_VALUES
                .iter()
                .position(|mode| *mode == cfg.keystroke_display.display_position)
                .unwrap_or(1) as isize;
            let _: () = msg_send![
                u.keystroke_display_position,
                selectItemAtIndex: display_position_idx
            ];
            let initial_position_idx = super::KEYSTROKE_DISPLAY_INITIAL_POSITION_VALUES
                .iter()
                .position(|edge| *edge == cfg.keystroke_display.initial_position)
                .unwrap_or(1) as isize;
            let _: () = msg_send![
                u.keystroke_display_initial_position,
                selectItemAtIndex: initial_position_idx
            ];
            // The cap slider follows a reload or a defaults restore like every other control.
            let max_extent = crate::config::keystroke_display_max_extent_percent_snap(
                cfg.keystroke_display.max_extent_percent,
            );
            let _: () =
                msg_send![u.keystroke_display_max_extent, setIntegerValue: max_extent as isize];
            super::page_builder::set_max_extent_readout(
                u.keystroke_display_max_extent_value,
                max_extent,
            );

            let thumbnail_idx: isize = if cfg.layout.thumbnails_enabled { 1 } else { 0 };
            let _: () = msg_send![u.thumbnails_enabled, selectItemAtIndex: thumbnail_idx];
            // The size popup has to follow a reload in place as well: the overlay reads the new
            // value on its next layout, so a stale selection would show one size while the switcher
            // uses another.
            let thumbnail_size_idx =
                crate::config::thumbnail_size_index_of(&cfg.layout.thumbnail_size).unwrap_or(0)
                    as isize;
            let _: () = msg_send![u.thumbnail_size, selectItemAtIndex: thumbnail_size_idx];
            let prewarm_state = if cfg.layout.focused_thumbnail_prewarm {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![u.focused_thumbnail_prewarm, setState: prewarm_state];
            // The band is driven by the size, so the slider and its readout are the controls that must
            // follow a reload; without this the page keeps showing the old point size (or 13 after a
            // saved 0) while the overlay already draws the new band.
            // size_value is a stop index; the readout shows the point size it means.
            let size_value = super::status_bar_size_slider_value(cfg.fonts.status_bar_size);
            let _: () = msg_send![u.status_bar_text_size, setIntegerValue: size_value];
            let readout =
                make_nsstring(&format!("{}", super::status_bar_size_for_index(size_value)));
            let _: () = msg_send![u.status_bar_text_size_value_label, setStringValue: readout];
            CFRelease(readout as *const c_void);
            // Same for the clipboard image-rows slider: the picker rebuilds its rows from the
            // reloaded config, so the settings control must show the same stop.
            let image_rows = (cfg.clipboard.image_rows as i64).clamp(1, 3);
            let _: () = msg_send![u.clipboard_image_rows, setIntegerValue: image_rows];
            set_field(u.clipboard_image_rows_value_label, image_rows);
            // The pushback: selection after pinning, and where the picker opens on its screen, both
            // follow an external edit or a defaults restore like every other control -- the next
            // summon already uses the new value, and the open page must not claim otherwise.
            let pin_follow_idx: isize = if cfg.clipboard.pin_follow_selection {
                0
            } else {
                1
            };
            let _: () = msg_send![u.clipboard_pin_follow, selectItemAtIndex: pin_follow_idx];
            let placement_idx = picker_placement_index(&cfg.clipboard.picker_placement);
            let _: () = msg_send![u.clipboard_picker_placement, selectItemAtIndex: placement_idx];
            // The finder menu page follows a reload or a defaults restore like every other page:
            // switches, layout popup, status line and the master-switch freeze.
            fill_finder_menu_controls(u, &cfg);
            // The display mode may have just changed: recompute the thumbnail-only pair.
            update_display_mode_dependent_visibility(u);
            // Key bindings: an external change (a menu toggle, a reloaded file) has to reach the
            // list. The sync rebuilds the rows only when the list's shape changed, so flipping a
            // row's switch from its own action does not destroy the control mid-action.
            super::bindings::sync_from_config(u, &cfg);
        });
    }
    // Outside the borrow (the refill re-borrows the settings slot): if the mouse page is the one on screen,
    // fill it from the reloaded config -- including the mapping rows and the device popup, which the old
    // in-place patch skipped while it still marked the page clean.
    super::materialize_mouse_page_if_visible();
}

/// Mapping from popup-item index to DeviceKey (1:1 with popup items), used by
/// handle_device_changed to look up the selected device by indexOfSelectedItem. Rebuilt each time
/// rebuild_device_popup runs. Contains only concrete devices; no "All Mice" wildcard entry.
static DEVICE_POPUP_KEYS: Mutex<Vec<crate::mouse::device::DeviceKey>> = Mutex::new(Vec::new());

/// Initialize/calibrate SELECTED_DEVICE against the current connected-device list. Must run
/// before resolve_selected() so resolution always gets a valid device:
/// - uninitialized (first settings open) -> select the first device (if any)
/// - initialized but the selected device was unplugged -> fall back to the first device
/// - selected device still present -> keep it
///
/// With no devices connected, clears the selection (edits the "All Mice" base layer).
/// Recalibrated on every settings open, so hot-plug (device add/remove) is handled naturally.
pub(super) fn ensure_selected_device() {
    let connected = crate::mouse::device::connected_devices();
    let cur = current_selected_device();

    if connected.is_empty() {
        // No device connected: clear the selection (edits the "All Mice" base layer).
        *SELECTED_DEVICE.lock().unwrap() = Some(None);
        return;
    }

    // Keep the current device if it's still connected; otherwise (uninitialized or unplugged)
    // fall back to the first device.
    let still_connected = cur
        .map(|c| {
            connected
                .iter()
                .any(|d| d.vendor_id == c.0 && d.product_id == c.1)
        })
        .unwrap_or(false);
    if !still_connected {
        let first = &connected[0];
        *SELECTED_DEVICE.lock().unwrap() = Some(Some((first.vendor_id, first.product_id)));
    }
}

/// Rebuild the device popup's items: only each connected device (no "All Mice" wildcard entry).
/// UI only (items + selection); SELECTED_DEVICE state calibration is handled by
/// ensure_selected_device. Called by load_settings_values (refreshed on each settings open to
/// reflect hot-plug changes).
pub(super) fn device_display_name(device: &crate::mouse::device::DeviceIdentity) -> String {
    if (device.vendor_id, device.product_id) == crate::mouse::device::VIRTUAL_DEVICE_KEY {
        crate::i18n::tf(
            "settings.virtual_mouse_label",
            &[("name", device.name.as_str())],
        )
    } else {
        device.name.clone()
    }
}

pub(super) fn device_info_caption(device: Option<&crate::mouse::device::DeviceIdentity>) -> String {
    let Some(device) = device else {
        return t("settings.no_device_detected");
    };
    if (device.vendor_id, device.product_id) == crate::mouse::device::VIRTUAL_DEVICE_KEY {
        t("settings.virtual_mouse_caption")
    } else {
        tf(
            "settings.device_hardware_id",
            &[
                ("vendor", &format!("{:04X}", device.vendor_id)),
                ("product", &format!("{:04X}", device.product_id)),
            ],
        )
    }
}

pub(super) fn device_info_caption_for_key(key: Option<crate::mouse::device::DeviceKey>) -> String {
    let connected = crate::mouse::device::connected_devices();
    let device = key.and_then(|key| {
        connected
            .iter()
            .find(|device| (device.vendor_id, device.product_id) == key)
    });
    device_info_caption(device)
}

pub(super) unsafe fn rebuild_device_popup(ui: &SettingsUi) {
    let connected = crate::mouse::device::connected_devices();
    let cur = current_selected_device();

    // Build the popup items and the key mapping: devices only.
    let mut items: Vec<String> = Vec::new();
    let mut keys: Vec<crate::mouse::device::DeviceKey> = Vec::new();
    for d in &connected {
        let key = (d.vendor_id, d.product_id);
        items.push(device_display_name(d));
        keys.push(key);
    }

    // Clear old items and fill in the new ones.
    let _: () = msg_send![ui.device_indicator, removeAllItems];
    for s in &items {
        let ns = make_nsstring(s);
        let _: () = msg_send![ui.device_indicator, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }

    // Select the item matching the current device; if it's gone, select the first.
    let sel_idx = cur
        .and_then(|c| keys.iter().position(|k| *k == c))
        .unwrap_or(0);
    if !keys.is_empty() {
        let _: () = msg_send![ui.device_indicator, selectItemAtIndex: sel_idx as isize];
    } else {
        // Keep one explicit, non-selectable status item when the list is empty so the popup
        // does not look like a failed or incomplete load.
        let ns = make_nsstring(&t("settings.no_device_detected"));
        let _: () = msg_send![ui.device_indicator, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }
    let caption = keys
        .get(sel_idx)
        .map(|key| device_info_caption_for_key(Some(*key)))
        .unwrap_or_else(|| device_info_caption(None));
    if !ui.device_info_caption.is_null() {
        set_field(ui.device_info_caption, &caption);
    }
    let _: () = msg_send![ui.device_indicator, setEnabled: !keys.is_empty()];

    *DEVICE_POPUP_KEYS.lock().unwrap() = keys;
}

/// Refresh the device popup live while the settings window is open (called on the main
/// thread from device plug/unplug events). The device list is external live state (hardware
/// attach/detach), not part of the OK/Cancel-gated preferences -- a reconnect should show
/// immediately without OK or reopening. The rebuild restores the selection from the in-memory
/// SELECTED_DEVICE, so unsaved choices survive. No-op when the window isn't open (it is
/// rebuilt on next open via load_settings_values anyway).
pub(crate) fn refresh_device_popup_if_open() {
    // The device list is an input of the mouse page, and this path only rebuilds the popup -- and only
    // while the window is visible. A hot-plug with the window hidden must still invalidate the page, or the
    // next open would show the device list it had before the change.
    super::note_mouse_page_dirty();
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                let visible: bool = msg_send![u.window, isVisible];
                if visible {
                    rebuild_device_popup(u);
                }
            }
        });
    }
}

/// Device-popup selection-changed callback: update SELECTED_DEVICE and immediately refresh the
/// other controls with the newly-selected device's effective values.
pub(crate) extern "C" fn handle_device_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("handle_device_changed", || {
        let popup = sender as *mut AnyObject;
        let idx: isize = unsafe { msg_send![popup, indexOfSelectedItem] };
        // DEVICE_POPUP_KEYS: Vec<DeviceKey>; get the key for the selected item (all concrete
        // devices; no wildcard entry).
        let new_dev = DEVICE_POPUP_KEYS.lock().unwrap().get(idx as usize).copied();
        *SELECTED_DEVICE.lock().unwrap() = Some(new_dev);
        // Only refresh the mouse page's per-device controls -- a full load_settings_from would
        // reset the enable_mouse switch to the saved cfg.mouse.enabled, wiping the user's
        // unsaved toggle (enable mouse control is a global setting; device switches must not
        // touch it).
        let cfg = CONFIG.read().unwrap().clone();
        let resolved = resolve_selected_from(&cfg);
        unsafe {
            with_settings_ui(|ui_guard| {
                if let Some(u) = ui_guard.as_mut() {
                    if !u.device_info_caption.is_null() {
                        set_field(u.device_info_caption, device_info_caption_for_key(new_dev));
                    }
                    fill_mouse_device_controls(u, &resolved);
                    // Keep the user's current enable_mouse state; only recompute freeze + visibility.
                    update_mouse_controls_enabled(u);
                    update_mode_dependent_visibility(u);
                    update_pointer_accel_visibility(u);
                    super::note_mouse_page_materialized_after_direct_fill();
                    // Device switch: reload the in-edit mappings from the new device's own profile.
                    let dev = current_selected_device();
                    let prof_idx = find_profile_index(&cfg, dev);
                    *MAPPING_EDITS.lock().unwrap() = prof_idx
                        .map(|i| cfg.mouse.profiles[i].button_mappings.clone())
                        .unwrap_or_default();

                    render_mapping_rows_locked(u);
                }
            });
        }
    });
}

// Sidebar click callback: read the sender's tag and switch to that page.

pub(crate) extern "C" fn on_sidebar_select(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("on_sidebar_select", || {
        // Navigating away also cancels an unfinished destructive confirmation.
        collapse_restore_confirmations(true);
        // A nav click means "show me this page": it leaves the search results first, or the query
        // stays active and keeps the pages covered.
        sidebar::clear_settings_search();
        let btn = sender as *mut AnyObject;
        let tag: isize = unsafe { msg_send![btn, tag] };
        select_sidebar(tag as usize);
    });
}

/// The settings "export logs" button: copy the active log file to a user-chosen
/// NSSavePanel destination. The read yields a point-in-time snapshot; the background
/// writer keeps appending to the original file, so the two never interfere.
pub(crate) extern "C" fn handle_export_logs(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("handle_export_logs", || {
        let Some(source) = crate::logger::active_log_path() else {
            super::window::show_alert(
                &t("settings.export_failed_title"),
                &t("settings.export_failed_no_source"),
            );
            return;
        };
        // a cancelled save panel is a silent no-op
        let destination = match unsafe { run_save_panel(&suggested_export_log_name()) } {
            Some(dest) => dest,
            None => return,
        };
        let outcome = std::fs::read(&source)
            .map_err(|e| e.to_string())
            .and_then(|bytes| std::fs::write(&destination, bytes).map_err(|e| e.to_string()));
        match outcome {
            Ok(()) => super::window::show_alert(
                &t("settings.export_done_title"),
                &tf("settings.export_done_msg", &[("path", &destination)]),
            ),
            Err(reason) => super::window::show_alert(
                &t("settings.export_failed_title"),
                &tf("settings.export_failed_msg", &[("reason", &reason)]),
            ),
        }
    });
}

/// Suggested export filename: oh-my-tab-YYYYMMDD-HHMMSS.log. The export is a point-in-time
/// snapshot; the timestamp keeps repeated exports from clobbering each other.
fn suggested_export_log_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "oh-my-tab-{:04}{:02}{:02}-{:02}{:02}{:02}.log",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        )
    }
}

// MARK: - Settings export / import

/// The export's local-time stamp: `YYYY-MM-DD HH.MM.SS`. A point-in-time snapshot, so repeated
/// exports never clobber each other.
fn local_timestamp_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::ffi::format_local_timestamp(secs)
}

/// Suggested export filename: the fixed prefix plus the local timestamp, e.g.
/// `oh-my-tab_backup_2026-10-14 11.54.20.toml`.
///
/// The stamp is the shared local-time format the clipboard's save-as uses, so a saved file reads
/// correctly next to Finder's own clock.
fn suggested_export_config_name() -> String {
    export_file_name(EXPORT_FILE_PREFIX, &local_timestamp_stamp())
}

/// The prefix the export file is named after.
///
/// A fixed string rather than the bundle's own name: the file is meant to be recognized and shared,
/// and a bundle-derived name would rename it in the development bundle ("Oh-My-Tab Dev") and in any
/// localized build, so the same user would get differently named backups from different builds. It is
/// lowercase and underscore-separated so a backup sorts and reads like the other files this app
/// writes (`oh-my-tab.log`), and so it never looks like the app bundle itself.
const EXPORT_FILE_PREFIX: &str = "oh-my-tab_backup";

/// The export filename for a prefix and stamp. Pure, so the shape is asserted without FFI; the live
/// path only supplies the two inputs.
fn export_file_name(prefix: &str, stamp: &str) -> String {
    format!("{prefix}_{stamp}.toml")
}

/// The settings "Export settings" button: write the whole live configuration to a user-chosen
/// NSSavePanel destination.
///
/// The snapshot is taken and rendered before the panel opens, so a config edit made while the
/// panel is up cannot land half-written in the file. The write itself runs on a worker: the
/// destination is wherever the user pointed, which may be a network volume or a slow external disk,
/// and an atomic write there includes an fsync that can take seconds. The outcome comes back
/// through `handleExportSettingsReady:`.
pub(crate) extern "C" fn handle_export_settings(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_export_settings", || {
        let config = CONFIG.read().unwrap().clone();
        let version = unsafe { bundle_info_string("CFBundleShortVersionString") };
        let content = portability::export_toml(&config, &version, &local_timestamp_stamp());
        // A cancelled save panel is a silent no-op.
        let Some(destination) = (unsafe { run_save_panel(&suggested_export_config_name()) }) else {
            return;
        };
        // No single-flight gate here: the exported bytes and the destination are both private to this
        // call, so two exports cannot interfere and the later completion simply wins the toast.
        if !spawn_worker("settings-export", move || {
            let outcome =
                crate::config::atomic_write(std::path::Path::new(&destination), content.as_bytes())
                    .map(|()| destination);
            *PENDING_EXPORT.lock().unwrap() = Some(outcome);
            notify_main(sel!(handleExportSettingsReady:));
        }) {
            show_alert(
                &t("settings.export_settings_failed_title"),
                &t("settings.worker_unavailable"),
            );
        }
    });
}

/// Main-thread continuation of "Export settings": report where the file landed, or why it did not.
pub(crate) extern "C" fn handle_export_settings_ready(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_export_settings_ready", || {
        let Some(outcome) = PENDING_EXPORT.lock().unwrap().take() else {
            return;
        };
        match outcome {
            Ok(destination) => {
                log_info!("[settings] exported configuration to {}", destination);
                show_settings_toast(&t("settings.toast_settings_exported"));
            }
            Err(reason) => show_alert(
                &t("settings.export_settings_failed_title"),
                &tf(
                    "settings.export_settings_failed_msg",
                    &[("reason", &reason)],
                ),
            ),
        }
    });
}

/// Start a background worker, reporting a spawn failure instead of panicking.
///
/// `.expect()` is not an option here: every one of these callers runs inside an `extern "C"`
/// AppKit callback, where a panic cannot unwind and would abort the whole app. A spawn failure is a
/// reportable error like any other.
fn spawn_worker(name: &str, work: impl FnOnce() + Send + 'static) -> bool {
    match std::thread::Builder::new().name(name.into()).spawn(work) {
        Ok(_) => true,
        Err(error) => {
            log_info!("[settings] could not start the {} worker: {}", name, error);
            false
        }
    }
}

/// The export result, handed from the worker to the main thread.
static PENDING_EXPORT: Mutex<Option<Result<String, String>>> = Mutex::new(None);

/// The settings "Import settings" button: ask for a file, then hand the reading to a worker.
///
/// Reading and parsing happen off the main thread: the user can pick a file on a network volume or
/// a slow disk, and doing that inside the AppKit callback would freeze the settings window for as
/// long as the read takes. The parsed result comes back through `handleImportSettingsReady:`.
pub(crate) extern "C" fn handle_import_settings(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_import_settings", || {
        // One import at a time, held from before the panel opens until the result has been applied and
        // reported. Without this, two clicks start two workers that share one result slot: the slower
        // one would overwrite the faster one's outcome, and whatever applied last would win rather than
        // whichever the user chose last.
        if IMPORT_IN_FLIGHT.swap(true, Ordering::SeqCst) {
            return;
        }
        // The flag is released in exactly one place, the ready callback, which also covers "the user
        // cancelled the panel" and every failure between here and the worker finishing.
        // The panel runs its own modal loop; no CONFIG lock is held across it.
        let Some(path) = (unsafe { run_open_panel() }) else {
            IMPORT_IN_FLIGHT.store(false, Ordering::SeqCst);
            return;
        };
        if !spawn_worker("settings-import", move || {
            // Read, parse and persist all happen here, off the main thread: the picked file may be on a
            // slow volume, and committing waits for an fsync.
            let outcome = match read_import_file(&path) {
                Ok(content) => match portability::parse_export(&content) {
                    Ok(parsed) => match commit_imported_config(parsed, "file") {
                        Ok((old, next, repaired)) => ImportOutcome::Committed {
                            old: Box::new(old),
                            next: Box::new(next),
                            repaired,
                        },
                        Err(reason) => ImportOutcome::CommitFailed(reason),
                    },
                    Err(error) => ImportOutcome::Refused(error),
                },
                Err(error) => ImportOutcome::Unreadable(error),
            };
            *PENDING_IMPORT.lock().unwrap() = Some(outcome);
            // Wake the main thread; the outcome is picked up there. Reading MENU_TARGET off-main
            // would trip the main-thread assertion, so this goes through the dispatch handle.
            notify_main(sel!(handleImportSettingsReady:));
        }) {
            IMPORT_IN_FLIGHT.store(false, Ordering::SeqCst);
            show_alert(
                &t("settings.import_settings_failed_title"),
                &t("settings.worker_unavailable"),
            );
        }
    });
}

/// Whether an import is currently in flight. A plain flag rather than a counter: the second
/// request is refused outright, so there is never more than one worker to account for.
static IMPORT_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

/// What the import worker found. Every variant is Send: it is produced off the main thread and
/// consumed on it.
enum ImportOutcome {
    /// Already persisted. Carries the config that was replaced so the main thread can replay the
    /// runtime change. Boxed: a whole `Config` is about a kilobyte, and every other variant is a few
    /// dozen bytes, so storing them inline would make the enum that size on the failure paths too.
    Committed {
        old: Box<Config>,
        next: Box<Config>,
        repaired: bool,
    },
    /// The file parsed but could not be written to disk; nothing was published.
    CommitFailed(String),
    Refused(portability::ImportError),
    Unreadable(ImportReadError),
}

/// The worker's result, handed from the import thread to the main thread. A mutex rather than a
/// main-thread slot because the producer is off-main.
static PENDING_IMPORT: Mutex<Option<ImportOutcome>> = Mutex::new(None);

/// Main-thread continuation of "Import settings": report a refusal, or install the configuration.
///
/// `apply_imported_config` first compares the candidate against the live config, but it still
/// persists: "the candidate equals the live config" says nothing about the file on disk (a hand
/// edit that has not been reloaded is exactly that case), so skipping the write would report success
/// for an import that reverts on the next launch.
pub(crate) extern "C" fn handle_import_settings_ready(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_import_settings_ready", || {
        // Release the single-flight gate at the top, before any early return: the flag must not
        // outlive this callback, or every later import would be refused as "already in flight".
        IMPORT_IN_FLIGHT.store(false, Ordering::SeqCst);
        let Some(outcome) = PENDING_IMPORT.lock().unwrap().take() else {
            return;
        };
        match outcome {
            ImportOutcome::Refused(error) => show_alert(
                &t("settings.import_settings_failed_title"),
                &import_error_message(&error),
            ),
            ImportOutcome::Unreadable(error) => show_alert(
                &t("settings.import_settings_failed_title"),
                &read_error_message(&error),
            ),
            ImportOutcome::CommitFailed(reason) => show_alert(
                &t("settings.import_settings_failed_title"),
                &tf(
                    "settings.import_settings_failed_msg",
                    &[("reason", &reason)],
                ),
            ),
            ImportOutcome::Committed {
                old,
                next,
                repaired,
            } => {
                // The disk write already succeeded on the worker; only the runtime side effects and the
                // window refresh are left for the main thread.
                apply_imported_runtime(&old, &next);
                // Partial success: the loader keeps every valid field and falls back on the rest, which
                // the user has to be told. It gets the alert rather than the toast because the
                // explanation does not fit the toast's fixed single line.
                if repaired {
                    show_alert(
                        &t("settings.import_settings_partial_title"),
                        &t("settings.import_settings_partial_msg"),
                    );
                } else {
                    show_settings_toast(&t("settings.toast_settings_imported"));
                }
            }
        }
    });
}

/// Upper bound on an imported file. A settings export is a few kilobytes; anything far past that
/// is not one, and reading it whole would put an arbitrary file into memory.
pub(super) const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ImportReadError {
    /// The path is not a regular file (a directory, a device, a socket).
    NotAFile,
    /// The file grew past [`MAX_IMPORT_BYTES`], whenever it was measured.
    TooLarge,
    /// The bytes are not valid UTF-8 text.
    NotText,
    Io(String),
}

/// Read an import file with the bound applied to the bytes actually read.
///
/// The file is opened once and read through a `take` of one byte past the cap, so a file that grows
/// (or that is swapped) after any earlier size check is still refused instead of read whole. The
/// handle's own metadata decides the file type, which is what tells a directory or a device from a
/// document.
pub(super) fn read_import_file(path: &str) -> Result<String, ImportReadError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| ImportReadError::Io(e.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|e| ImportReadError::Io(e.to_string()))?;
    if !metadata.is_file() {
        return Err(ImportReadError::NotAFile);
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(MAX_IMPORT_BYTES) as usize + 1);
    file.take(MAX_IMPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ImportReadError::Io(e.to_string()))?;
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(ImportReadError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| ImportReadError::NotText)
}

/// The user-facing message for a file that could not be read.
fn read_error_message(error: &ImportReadError) -> String {
    match error {
        ImportReadError::NotAFile => t("settings.import_settings_not_a_file"),
        ImportReadError::TooLarge => t("settings.import_settings_too_large"),
        // A binary file is not something the user can act on differently from a corrupt one.
        ImportReadError::NotText => t("settings.import_settings_unreadable"),
        ImportReadError::Io(reason) => {
            tf("settings.import_settings_failed_msg", &[("reason", reason)])
        }
    }
}

/// The user-facing message for a refused file. Deliberately built from i18n keys rather than the
/// parser error: the loader's own errors name internal field paths and internal enum values, and
/// those must never be shown.
fn import_error_message(error: &portability::ImportError) -> String {
    match error {
        portability::ImportError::NotAnExport => t("settings.import_settings_not_an_export"),
        portability::ImportError::UnsupportedFormat(version) => tf(
            "settings.import_settings_newer_format",
            &[("version", &version.to_string())],
        ),
        portability::ImportError::Unreadable => t("settings.import_settings_unreadable"),
    }
}

/// Install a parsed export: keep the machine-local fields, write and publish the result as one
/// transaction, then replay every runtime side effect and refresh the open window.
///
/// Returns whether the loader repaired anything (the caller reports a partial success).
/// Commit an imported configuration on the calling (worker) thread, returning the previous config so
/// the main thread can replay the change.
///
/// Persisting here rather than on the main thread matters: `commit_config` waits for an fsync, and
/// the destination config directory can sit on slow or network storage. Doing it in the reader
/// thread keeps the settings window responsive.
///
/// The candidate is derived from the live config read at this moment, so the machine-local fields
/// are taken from the config the import is actually about to replace.
fn commit_imported_config(
    parsed: portability::ImportedConfig,
    source: &str,
) -> Result<(Config, Config, bool), String> {
    let old = CONFIG.read().unwrap().clone();
    let mut next = parsed.config;
    portability::restore_local_fields(&mut next, &old);
    // Persist FIRST and unconditionally: "the candidate equals the live config" does not mean the
    // file on disk agrees. A hand-edited config.toml that has not been reloaded is exactly that
    // case, and skipping the write there would report success for an import that reverts on the
    // next launch. The write also has to succeed before anything is published or applied.
    crate::config::commit_config(next.clone())?;
    log_info!("[settings] imported configuration from {}", source);
    Ok((old, next, parsed.repaired))
}

/// Replay the runtime side effects of an import on the main thread.
pub(crate) fn apply_imported_runtime(old: &Config, next: &Config) {
    if next == old {
        // Nothing observable changed, so no runtime side effect has to be replayed and the window
        // does not need rebuilding -- the disk write was the whole point.
        return;
    }
    // One shared decision: `apply_config_change` rebuilds the settings content exactly when this is
    // true. A material-only change no longer rebuilds, so the old "appearance changed => already
    // rebuilt" inference would leave the other imported controls stale.
    let rebuilt = crate::runtime_config::settings_window_rebuild_required(old, next);
    apply_config_change(old, next, ConfigChangeSource::Reload);
    // When it did not rebuild, refill every control from the imported config in place.
    // `load_settings_values` also covers fields no narrow sync does (the log level dropdown, the
    // mouse page) and does not rebuild.
    if !rebuilt {
        super::load_settings_values();
    }
}

/// Show a transient success bubble in whichever settings window is open.
fn show_settings_toast(text: &str) {
    let window =
        with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)).unwrap_or(std::ptr::null_mut());
    if window.is_null() {
        return;
    }
    unsafe { tooltip::SettingsTooltip::show_success_bubble(window, text) };
}

#[cfg(test)]
mod portability_ui_tests {

    /// The import path must never hand a raw loader error to the user: those name internal field
    /// paths and internal enum values. Each refusal has its own message, and the unsupported
    /// version names the version it saw.
    #[test]
    fn every_import_refusal_has_its_own_message() {
        let not_an_export =
            super::import_error_message(&super::portability::ImportError::NotAnExport);
        let unreadable = super::import_error_message(&super::portability::ImportError::Unreadable);
        let newer =
            super::import_error_message(&super::portability::ImportError::UnsupportedFormat(2));
        for message in [&not_an_export, &unreadable, &newer] {
            assert!(!message.is_empty(), "a refusal must say something");
            assert!(
                !message.starts_with("settings."),
                "a missing locale key must not reach the user: {message}"
            );
        }
        assert_ne!(
            not_an_export, unreadable,
            "the two refusals must be distinguishable"
        );
        assert!(
            newer.contains("2"),
            "the unsupported version must be named: {newer}"
        );
    }

    /// The filename is the fixed prefix plus the local timestamp, and the extension is what the save
    /// panel shows and what a user later recognizes.
    #[test]
    fn the_export_name_is_the_prefix_plus_a_local_stamp() {
        // A pure example pins the shape, the underscore separator and the lowercase prefix...
        assert_eq!(
            super::export_file_name("oh-my-tab_backup", "2026-10-14 11.54.20"),
            "oh-my-tab_backup_2026-10-14 11.54.20.toml"
        );
        // ...and the stamp itself is the shared local-time format, never a bare epoch or a UTC
        // rendering: `YYYY-MM-DD HH.MM.SS` (dots, because colons are reserved in Finder names).
        let stamp = super::local_timestamp_stamp();
        assert_eq!(stamp.len(), 19, "got {stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-', "got {stamp}");
        assert_eq!(stamp.as_bytes()[7], b'-', "got {stamp}");
        assert_eq!(stamp.as_bytes()[10], b' ', "got {stamp}");
        assert_eq!(stamp.as_bytes()[13], b'.', "got {stamp}");
        assert_eq!(stamp.as_bytes()[16], b'.', "got {stamp}");
    }

    /// The live filename really goes through the prefix and the stamp, so a caller cannot regress to
    /// a hardcoded slug without this failing.
    #[test]
    fn the_live_export_name_carries_the_prefix_and_a_stamp() {
        let name = super::suggested_export_config_name();
        // The stamp is the tail of the name, so stripping it leaves exactly the prefix. This fails on
        // the hardcoded slug the feature started with, which had neither.
        let suffix = format!("_{}.toml", super::local_timestamp_stamp());
        let prefix = name
            .strip_suffix(&suffix)
            .unwrap_or_else(|| panic!("{name} must end with the local stamp {suffix}"));
        assert!(!prefix.is_empty(), "the prefix must name the file: {name}");
        assert_eq!(
            prefix,
            super::EXPORT_FILE_PREFIX,
            "the prefix must name the file"
        );
    }

    /// The single-flight gate is what keeps two imports from fighting over one result slot: the
    /// second request is refused outright, so a slower first import can never be overwritten by a
    /// faster second one.
    #[test]
    fn a_second_import_is_refused_while_one_is_in_flight() {
        // Claiming is an atomic test-and-set, exactly as the button handler does it.
        assert!(!super::IMPORT_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst));
        assert!(
            super::IMPORT_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst),
            "a second click must see the gate already taken"
        );
        // Releasing lets the next import through: a cancelled panel, a refused file and a failed
        // commit all release it, or the feature would be dead after the first failure.
        super::IMPORT_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !super::IMPORT_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst),
            "the gate must reopen after the previous import finished"
        );
        super::IMPORT_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// A worker that cannot start is reported, never panicked on: these handlers run inside
    /// `extern "C"` AppKit callbacks, where a panic aborts the process instead of unwinding.
    #[test]
    fn a_worker_that_cannot_start_is_reported_not_panicked() {
        assert!(super::spawn_worker("omt-test-worker", || {}));
        let handle = std::thread::Builder::new()
            .name("omt-test-worker".into())
            .spawn(|| {})
            .expect("the first worker must start");
        handle.join().unwrap();
    }

    /// The publish gate is the whole point of the import transaction: a failed write must leave the
    /// running configuration exactly as it was.
    #[test]
    fn a_failed_write_never_publishes_the_candidate() {
        use crate::config::Config;
        let mut current = Config::default();
        current.appearance.theme = "dark".into();
        let mut candidate = current.clone();
        candidate.appearance.theme = "light".into();
        let failed = crate::config::publish_after_write(
            &mut current,
            candidate.clone(),
            Err("disk full".into()),
        );
        assert!(failed.is_err());
        assert_eq!(
            current.appearance.theme, "dark",
            "the live config must not adopt a candidate that failed to persist"
        );
        let ok = crate::config::publish_after_write(&mut current, candidate, Ok(()));
        assert!(ok.is_ok());
        assert_eq!(current.appearance.theme, "light");
    }

    /// The read bound is applied to the bytes actually read, not to a size measured beforehand.
    /// A file that grows between the two, or that is not a regular file at all, must be refused
    /// instead of pulled into memory.
    #[test]
    fn an_import_never_reads_past_its_bound() {
        let dir = std::env::temp_dir().join(format!("omt-import-bound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // A directory is not an import file.
        assert_eq!(
            super::read_import_file(&dir.to_string_lossy()),
            Err(super::ImportReadError::NotAFile)
        );

        // A file one byte past the cap is refused; one at the cap is read.
        let over = dir.join("over.toml");
        std::fs::write(&over, vec![b'a'; super::MAX_IMPORT_BYTES as usize + 1]).unwrap();
        assert_eq!(
            super::read_import_file(&over.to_string_lossy()),
            Err(super::ImportReadError::TooLarge)
        );

        let at = dir.join("at.toml");
        std::fs::write(&at, vec![b'a'; super::MAX_IMPORT_BYTES as usize]).unwrap();
        assert_eq!(
            super::read_import_file(&at.to_string_lossy()).map(|s| s.len()),
            Ok(super::MAX_IMPORT_BYTES as usize)
        );

        // Non-UTF-8 bytes are a refusal, never a lossy decode into a half-valid document.
        let binary = dir.join("binary.toml");
        std::fs::write(&binary, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(
            super::read_import_file(&binary.to_string_lossy()),
            Err(super::ImportReadError::NotText)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every read failure has a message of its own, so the user is told which thing went wrong.
    #[test]
    fn every_read_failure_has_its_own_message() {
        let not_a_file = super::read_error_message(&super::ImportReadError::NotAFile);
        let too_large = super::read_error_message(&super::ImportReadError::TooLarge);
        let not_text = super::read_error_message(&super::ImportReadError::NotText);
        let io = super::read_error_message(&super::ImportReadError::Io("nope".into()));
        for message in [&not_a_file, &too_large, &not_text, &io] {
            assert!(!message.is_empty());
            assert!(
                !message.starts_with("settings."),
                "a missing locale key must not reach the user: {message}"
            );
        }
        assert_ne!(not_a_file, too_large);
        assert_ne!(not_a_file, not_text);
    }
}

#[cfg(test)]
mod tests {
    /// The popup's items and the stored strings are one mapping, and it must survive a round trip.
    ///
    /// Counter-example this is built to fail on: reordering `PICKER_PLACEMENT_VALUES` shows "Follow
    /// the Cursor" while storing `center`. Every other layer (config, page builder, runtime flags)
    /// passes through that array, so nothing else would notice the swap.
    #[test]
    fn the_placement_popup_round_trips_every_value() {
        for value in super::PICKER_PLACEMENT_VALUES {
            let index = super::picker_placement_index(value);
            assert_eq!(
                super::picker_placement_for_popup_index(index),
                value,
                "{value} must come back from the popup index it selects"
            );
        }
        // The two values are distinct, so the array cannot collapse into one menu item.
        assert_ne!(
            super::picker_placement_index("cursor"),
            super::picker_placement_index("center")
        );
        // An unknown stored value shows the default and saves the default, never a third mode.
        let fallback = super::picker_placement_index("sideways");
        assert_eq!(
            super::picker_placement_for_popup_index(fallback),
            crate::config::ClipboardSection::default().picker_placement
        );
        // An index past the end (a menu rebuilt while a stale value was selected) does the same.
        assert_eq!(
            super::picker_placement_for_popup_index(99),
            super::PICKER_PLACEMENT_VALUES[0]
        );
    }

    use super::{
        checked_ids, clipboard_delete_dependent_visibility_from_config, smooth_slider_display,
        smooth_slider_value, take_edited_field, text_field_bit, Config, Ordering, TextField,
    };

    #[test]
    fn clipboard_delete_dependent_row_matches_initial_config() {
        let mut cfg = Config::default();
        cfg.clipboard.clear_system_pasteboard_after_paste = true;
        assert!(!clipboard_delete_dependent_visibility_from_config(&cfg));

        cfg.clipboard.delete_after_paste = true;
        assert!(clipboard_delete_dependent_visibility_from_config(&cfg));
    }

    #[test]
    fn only_a_field_the_user_edited_commits_on_end_editing() {
        let mask = std::sync::atomic::AtomicU32::new(0);
        assert!(
            !take_edited_field(&mask, TextField::CornerRadius),
            "an untouched field must not commit (the prewarmed window resigns it on every open)"
        );
        mask.fetch_or(text_field_bit(TextField::CornerRadius), Ordering::SeqCst);
        assert!(
            take_edited_field(&mask, TextField::CornerRadius),
            "a field the user typed into must commit on blur"
        );
        assert!(
            !take_edited_field(&mask, TextField::CornerRadius),
            "one edit must commit once, not on every later resign"
        );
        assert!(
            !take_edited_field(&mask, TextField::ClipboardMaxEntries),
            "the two fields must not share a bit"
        );
        mask.store(0, Ordering::SeqCst);
        mask.fetch_or(
            text_field_bit(TextField::ClipboardMaxEntries),
            Ordering::SeqCst,
        );
        assert!(take_edited_field(&mask, TextField::ClipboardMaxEntries));
    }

    /// The Finder Menu's vec-backed fields keep the catalog order however the boxes were ticked:
    /// the menu renders the stored vec as its item order, so click order must never leak into it.
    #[test]
    fn finder_menu_checked_ids_keep_catalog_order() {
        let file_types = ["md", "txt", "json"];
        assert_eq!(
            checked_ids(file_types, &[true, false, true]),
            vec!["md".to_string(), "json".to_string()]
        );
        assert!(checked_ids(file_types, &[false, false, false]).is_empty());
        // A states slice of a different length cannot invent or add ids.
        assert_eq!(checked_ids(["md"], &[true, true]), vec!["md".to_string()]);
        assert_eq!(
            checked_ids(file_types, &[true, false]),
            vec!["md".to_string()]
        );
        // Bundle ids go through the same function, so terminal order is the catalog order too.
        let terminals: Vec<&str> = crate::finder_menu::terminals()
            .iter()
            .map(|spec| spec.bundle_id)
            .collect();
        let states: Vec<bool> = (0..terminals.len()).map(|index| index % 2 == 0).collect();
        let expected: Vec<String> = terminals
            .iter()
            .enumerate()
            .filter(|(index, _)| index % 2 == 0)
            .map(|(_, bundle_id)| bundle_id.to_string())
            .collect();
        assert_eq!(checked_ids(terminals.iter().copied(), &states), expected);
    }

    #[test]
    fn smooth_scroll_slider_rounds_to_one_decimal_and_clamps() {
        assert_eq!(smooth_slider_value(1.26, 0.0, 2.0), 1.3);
        assert_eq!(smooth_slider_value(-1.0, 0.0, 2.0), 0.0);
        assert_eq!(smooth_slider_display(8.04, 0.0, 8.0), "8.0");
        assert_eq!(smooth_slider_display(f64::NAN, 0.0, 2.0), "0.0");
    }
    /// Every field a control can report must have exactly one apply branch, and every control the
    /// freeze lists name must be one a control reports.
    ///
    /// Why a source scan: `control_field_of` matches control POINTERS by hand and `apply_control_field`
    /// matches the enum by hand, so a field can be added to one and not the other with nothing failing
    /// -- the setting then shows, animates and writes nothing (`on_control_changed` logs "unknown sender"
    /// and returns). The pointer table is invisible to the type system, which is exactly why the
    /// settings checklist exists; this makes its step 10/11 machine-checked.
    #[test]
    fn every_control_field_is_wired_in_both_directions() {
        // Read the file instead of `include_str!`: the scan would otherwise see this test's own
        // source, and the string literals below would satisfy its own search.
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/settings/dispatch.rs"
        ))
        .expect("dispatch.rs");
        // The enum's variants, from its declaration block.
        let enum_start = source.find("pub(super) enum ControlField {").expect("enum");
        let enum_end = source[enum_start..].find('}').expect("enum end") + enum_start;
        let body = &source[enum_start + 1..enum_end];
        let mut variants = Vec::new();
        for line in body.lines() {
            let line = line.trim();
            if line.starts_with("//") || line.is_empty() {
                continue;
            }
            // A variant line is `Name,` or `Name(Type)`: the identifier must START the line and be
            // followed by a comma or a paren. Blanking the leading word only (as an earlier version
            // did) picked `ub` out of a wrapped doc comment, and then reported a variant that does
            // not exist.
            let name: String = line
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let rest = line[name.len()..].trim_start();
            let looks_like_variant = line.starts_with(|c: char| c.is_ascii_alphabetic())
                && name.starts_with(|c: char| c.is_ascii_uppercase())
                && (rest.starts_with(',') || rest.starts_with('('));
            if !name.is_empty() && looks_like_variant {
                variants.push(name);
            }
        }
        // Counter-example: a wrapped doc comment whose continuation line begins with a word
        // must not be read as a variant. An earlier version of this parser reported `ub` out of
        // "…`handleBindingDelete:`), not" and then failed for a variant that does not exist.
        let mut found: Vec<&str> = variants.iter().map(String::as_str).collect();
        found.sort_unstable();
        assert!(
            !found.contains(&"ub"),
            "a doc-comment continuation must not parse as a variant: {found:?}"
        );
        assert!(found.contains(&"Theme") && found.contains(&"FinderMenuCopyPath"));
        assert!(
            found.contains(&"FinderMenuNewFileType"),
            "a tuple variant counts"
        );
        variants.sort();
        variants.dedup();
        assert!(
            variants.len() > 40,
            "the enum parse found only {} variants; it cannot decide",
            variants.len()
        );

        // Both blocks that must mention every variant: the pointer lookup and the apply match.
        // Cut the source at the test module: everything past it is this test's own text, and the
        // string literals below ("ControlField::…") would make every search find itself. An earlier
        // version searched the whole file, so `lookup` ran past into `apply_control_field` and a
        // deleted pointer mapping still passed.
        let production = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("production code");
        assert!(
            production.len() < source.len(),
            "the test module was not found; the scan would read its own literals"
        );
        let block_of = |start_marker: &str| -> String {
            let start = production
                .find(start_marker)
                .unwrap_or_else(|| panic!("`{start_marker}` not found"));
            // The function body ends at the first line that is exactly `}` at column 0.
            let rest = &production[start..];
            let end = rest.find("\n}\n").map(|end| end + 3).unwrap_or(rest.len());
            rest[..end].to_string()
        };
        let lookup = block_of("fn control_field_of(");
        let apply = block_of("fn apply_control_field(");
        // Counter-example against a mis-cut range: the two blocks must be disjoint, or a variant
        // found in one would satisfy the other.
        assert!(
            !apply.contains("fn control_field_of("),
            "the apply range swallowed the lookup function; the scan cannot decide"
        );
        let mut missing = Vec::new();
        for variant in &variants {
            let token = format!("ControlField::{variant}");
            if !lookup.contains(&token) {
                missing.push(format!("{variant}: no control_field_of branch"));
            }
            // The apply match names the variant bare (`ControlField::Theme => {`), so search the
            // qualified form there too.
            if !apply.contains(&token) {
                missing.push(format!("{variant}: no apply_control_field branch"));
            }
        }
        assert!(
            missing.is_empty(),
            "a ControlField without a branch shows a setting that does nothing:\n  {}",
            missing.join("\n  ")
        );
    }
}

// MARK: - Settings search

/// The sidebar search field's text changed: filter the settings live.
pub(crate) extern "C" fn handle_settings_search_changed(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut c_void,
) {
    callback_guard::void("handle_settings_search_changed", || {
        // A rebuild/hide running inside `with_settings_ui` may deliver this callback
        // re-entrantly (e.g. resigning a field's first responder). Borrow-check it: a dropped
        // notification loses one refresh; an abort loses the app.
        if crate::settings::settings_ui_borrowed() {
            return;
        }

        unsafe {
            let field: *mut AnyObject = msg_send![notification as *mut AnyObject, object];
            sidebar::settings_search_field_changed(field);
        }
    });
}

/// The search field began editing: the shell switches to the mock's focused ring.
pub(crate) extern "C" fn handle_settings_search_began_editing(
    _self: *mut c_void,
    _cmd: Sel,
    _notification: *mut c_void,
) {
    callback_guard::void("handle_settings_search_began_editing", || {
        sidebar::set_settings_search_focused(true);
    });
}

/// The search field stopped editing: the shell drops the ring.
///
/// Returning YES lets AppKit's end-of-editing notification continue normally.
pub(crate) extern "C" fn handle_settings_search_ended_editing(
    _self: *mut c_void,
    _cmd: Sel,
    _notification: *mut c_void,
) -> objc2::runtime::Bool {
    callback_guard::value(
        "handle_settings_search_ended_editing",
        objc2::runtime::Bool::YES,
        || {
            sidebar::set_settings_search_focused(false);
            objc2::runtime::Bool::YES
        },
    )
}

/// The search field's clear chip: empty the query and restore the page content.
pub(crate) extern "C" fn handle_settings_search_clear(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("handle_settings_search_clear", || {
        // A rebuild/hide running inside `with_settings_ui` may deliver this callback
        // re-entrantly (e.g. resigning a field's first responder). Borrow-check it: a dropped
        // notification loses one refresh; an abort loses the app.
        if crate::settings::settings_ui_borrowed() {
            return;
        }

        sidebar::clear_settings_search();
    });
}
