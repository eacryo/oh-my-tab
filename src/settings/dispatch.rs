//! 设置窗口 · dispatch:控件事件分发、即时写入 CONFIG 与联动启用/刷新。
//! Control-event dispatch, live CONFIG writes, and dependent enable/refresh logic.

use super::*;

// ========== 即时生效字段调度器 / live-apply control dispatcher ==========
// 所有设置修改即时写入内存 CONFIG 并调度防抖落盘;不再存在“点确认才生效”的
// pending/staged 状态。数字文本框允许输入过程中的临时非法值(仅合法时应用),
// 滑块拖动实时生效、磁盘写入防抖。
// Every change is written to the in-memory CONFIG immediately with a debounced disk write;
// no pending/staged state exists anymore. Numeric text fields may hold transient invalid
// values while typing (applied only when valid); sliders take effect live while drags
// persist with a debounce.

/// 可编辑控件标识。控件指针 → 字段的映射见 control_field_of。
/// Editable control ids. The pointer → field mapping lives in control_field_of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlField {
    Theme,
    GlassStyle,
    GlassTint,
    Locale,
    LogLevel,
    LaunchAtLogin,
    WindowsEnabled,
    ShowMinimized,
    ThumbnailsEnabled,
    FocusedThumbnailPrewarm,
    ShowAppNameInCards,
    CardTextSize,
    StatusBarTextSize,
    OverlayPosition,
    ActivationMode,
    CornerRadius,
    Modifier,
    MouseEnabled,
    ReverseScroll,
    ScrollMode,
    LineCount,
    DisablePointerAccel,
    PointerAcceleration,
    MappingEnabled,
    ClipboardEnabled,
    ClipboardPersist,
    ClipboardShowSourceApp,
    ClipboardMoveUsedToTop,
    ClipboardDeleteAfterPaste,
    ClipboardClearSystemPasteboardAfterPaste,
    ClipboardMaxEntries,
    ClipboardAutoExpireDays,
    ClipboardPinFollow,
    WindowControlEnabled,
    WindowControlUp,
    WindowControlDown,
    WindowControlLeft,
    WindowControlRight,
    WindowControlDisplayUp,
    WindowControlDisplayDown,
    WindowControlDisplayLeft,
    WindowControlDisplayRight,
    QuickActionsEnabled,
    QuickActionOpenSettings,
    QuickActionOpenFinder,
    QuickActionShowDesktop,
    QuickActionLockScreen,
    QuickActionLocatePointer,
    UpdateAutoCheck,
    UpdateAutoDownload,
}

/// 按控件指针识别字段(设置窗口复用,指针稳定)。
/// Identify a field by its control pointer (the window is reused, pointers are stable).
unsafe fn control_field_of(sender: *mut AnyObject) -> Option<ControlField> {
    with_settings_ui(|ui| {
        let u = ui.as_ref()?;
        let ptr = sender as usize;
        let m = |ctrl: *mut AnyObject, field: ControlField| (ptr == ctrl as usize).then_some(field);
        m(u.theme, ControlField::Theme)
            .or_else(|| m(u.glass_style, ControlField::GlassStyle))
            .or_else(|| m(u.glass_tint, ControlField::GlassTint))
            .or_else(|| m(u.locale, ControlField::Locale))
            .or_else(|| m(u.log_level, ControlField::LogLevel))
            .or_else(|| m(u.launch_at_login, ControlField::LaunchAtLogin))
            .or_else(|| m(u.windows_enabled, ControlField::WindowsEnabled))
            .or_else(|| m(u.show_minimized, ControlField::ShowMinimized))
            .or_else(|| m(u.thumbnails_enabled, ControlField::ThumbnailsEnabled))
            .or_else(|| {
                m(
                    u.focused_thumbnail_prewarm,
                    ControlField::FocusedThumbnailPrewarm,
                )
            })
            .or_else(|| m(u.show_app_name_in_cards, ControlField::ShowAppNameInCards))
            .or_else(|| m(u.card_text_size, ControlField::CardTextSize))
            .or_else(|| m(u.status_bar_text_size, ControlField::StatusBarTextSize))
            .or_else(|| m(u.overlay_position, ControlField::OverlayPosition))
            .or_else(|| m(u.activation_mode, ControlField::ActivationMode))
            .or_else(|| m(u.corner_radius, ControlField::CornerRadius))
            .or_else(|| m(u.modifier, ControlField::Modifier))
            .or_else(|| m(u.enable_mouse, ControlField::MouseEnabled))
            .or_else(|| m(u.reverse_scroll, ControlField::ReverseScroll))
            .or_else(|| m(u.scroll_mode, ControlField::ScrollMode))
            .or_else(|| m(u.line_count, ControlField::LineCount))
            .or_else(|| m(u.disable_pointer_accel, ControlField::DisablePointerAccel))
            .or_else(|| m(u.pointer_accel_slider, ControlField::PointerAcceleration))
            .or_else(|| m(u.mapping_enabled, ControlField::MappingEnabled))
            .or_else(|| m(u.clipboard_enabled, ControlField::ClipboardEnabled))
            .or_else(|| m(u.clipboard_persist, ControlField::ClipboardPersist))
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
            .or_else(|| m(u.clipboard_pin_follow, ControlField::ClipboardPinFollow))
            .or_else(|| m(u.window_control_enabled, ControlField::WindowControlEnabled))
            .or_else(|| m(u.window_control_up, ControlField::WindowControlUp))
            .or_else(|| m(u.window_control_down, ControlField::WindowControlDown))
            .or_else(|| m(u.window_control_left, ControlField::WindowControlLeft))
            .or_else(|| m(u.window_control_right, ControlField::WindowControlRight))
            .or_else(|| {
                m(
                    u.window_control_display_up,
                    ControlField::WindowControlDisplayUp,
                )
            })
            .or_else(|| {
                m(
                    u.window_control_display_down,
                    ControlField::WindowControlDisplayDown,
                )
            })
            .or_else(|| {
                m(
                    u.window_control_display_left,
                    ControlField::WindowControlDisplayLeft,
                )
            })
            .or_else(|| {
                m(
                    u.window_control_display_right,
                    ControlField::WindowControlDisplayRight,
                )
            })
            .or_else(|| m(u.quick_actions_enabled, ControlField::QuickActionsEnabled))
            .or_else(|| {
                m(
                    u.quick_actions_open_settings,
                    ControlField::QuickActionOpenSettings,
                )
            })
            .or_else(|| {
                m(
                    u.quick_actions_open_finder,
                    ControlField::QuickActionOpenFinder,
                )
            })
            .or_else(|| {
                m(
                    u.quick_actions_show_desktop,
                    ControlField::QuickActionShowDesktop,
                )
            })
            .or_else(|| {
                m(
                    u.quick_actions_lock_screen,
                    ControlField::QuickActionLockScreen,
                )
            })
            .or_else(|| {
                m(
                    u.quick_actions_locate_pointer,
                    ControlField::QuickActionLocatePointer,
                )
            })
            .or_else(|| m(u.update_auto_check, ControlField::UpdateAutoCheck))
            .or_else(|| m(u.update_auto_download, ControlField::UpdateAutoDownload))
    })
}

/// 给控件绑定统一回调(开关/下拉/滑块)。
/// Bind a control to the unified change callback (switches/popups/sliders).
pub(super) unsafe fn bind_control(target: *mut AnyObject, ctrl: *mut AnyObject) {
    let _: () = msg_send![ctrl, setTarget: target];
    let _: () = msg_send![ctrl, setAction: sel!(handleControlChanged:)];
}

/// 统一控件回调(开关/下拉/滑块/取色器):识别字段后应用其值。
/// The unified control callback (switches/popups/sliders/color well): identify the field and
/// apply its value.
pub(crate) extern "C" fn on_control_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    unsafe {
        let ctrl = sender as *mut AnyObject;
        // 滑块右侧数值 label 先行刷新(与旧回调一致)。
        // Refresh the slider value labels first (same as the old callbacks).
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                if ctrl == u.line_count {
                    let val: isize = msg_send![ctrl, integerValue];
                    set_field(u.line_count_value_label, val);
                } else if ctrl == u.card_text_size || ctrl == u.status_bar_text_size {
                    let val: isize = msg_send![ctrl, integerValue];
                    let label = if ctrl == u.card_text_size {
                        u.card_text_size_value_label
                    } else {
                        u.status_bar_text_size_value_label
                    };
                    set_field(label, val);
                } else if ctrl == u.clipboard_auto_expire_days {
                    let val: isize = msg_send![ctrl, integerValue];
                    set_field(u.clipboard_auto_expire_days_value_label, val);
                } else if ctrl == u.pointer_accel_slider {
                    // 指针加速 / 跟踪速度:只读数值随拖动实时刷新(2 位小数)。
                    // Pointer acceleration / tracking speed: the read-only value follows the drag
                    // in real time (2 decimals).
                    let val: f64 = msg_send![ctrl, doubleValue];
                    set_field(
                        u.pointer_accel_value_label,
                        pointer_accel_display(pointer_accel_from_slider(val)),
                    );
                }
            }
        });
        let Some(field) = control_field_of(ctrl) else {
            log_debug!("[settings] control change from an unknown sender ignored");
            return;
        };
        apply_control_field(field);
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
        if matches!(field, ControlField::ClipboardDeleteAfterPaste) {
            // "粘贴后删除条目"切换:它的子项(同时删除系统剪贴板条目)随之显隐。
            // The "delete entry after paste" switch flipped: its child option (clear the matching
            // system-pasteboard entry) follows.
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    update_clipboard_delete_dependent_visibility(u);
                }
            });
        }
        if matches!(field, ControlField::DisablePointerAccel) {
            // 开关切换:跟踪速度行随之显隐(打开=线性跟踪时才出现)。
            // The switch flipped: the tracking-speed row follows (it only appears while linear
            // tracking is on).
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_mut() {
                    update_pointer_accel_visibility(u);
                }
            });
        }
        if matches!(field, ControlField::ThumbnailsEnabled) {
            // 显示模式切换:仅缩略图模式的两行随之显隐。
            // The display mode flipped: the thumbnail-only pair follows.
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_mut() {
                    update_display_mode_dependent_visibility(u);
                }
            });
        }
    }
}

/// 读取控件值写入内存 CONFIG + 调度防抖落盘 + 即时副作用。
/// Read the control value into the in-memory CONFIG, schedule the debounced persist, and run
/// the field's immediate side effects.
fn apply_control_field(field: ControlField) {
    // 鼠标页 per-device 字段走 profile 通道(写选中设备档)。
    // Mouse-page per-device fields go through the profile channel (the selected device's profile).
    match field {
        ControlField::ReverseScroll
        | ControlField::ScrollMode
        | ControlField::LineCount
        | ControlField::DisablePointerAccel
        | ControlField::PointerAcceleration
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
                ControlField::GlassStyle => {
                    let idx: isize = msg_send![u.glass_style, indexOfSelectedItem];
                    cfg.appearance.glass_style = if idx == 1 { "clear" } else { "regular" }.into();
                }
                ControlField::GlassTint => {
                    let color: *mut AnyObject = msg_send![u.glass_tint, color];
                    if let Some(hex) = ns_color_to_hex(color) {
                        cfg.appearance.glass_tint = hex;
                    }
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
                ControlField::WindowsEnabled => {
                    let state: isize = msg_send![u.windows_enabled, state];
                    cfg.windows.enabled = state == 1;
                }
                ControlField::ShowMinimized => {
                    let state: isize = msg_send![u.show_minimized, state];
                    cfg.windows.show_minimized = state == 1;
                }
                ControlField::ThumbnailsEnabled => {
                    let idx: isize = msg_send![u.thumbnails_enabled, indexOfSelectedItem];
                    cfg.layout.thumbnails_enabled = idx == 1;
                }
                ControlField::FocusedThumbnailPrewarm => {
                    let state: isize = msg_send![u.focused_thumbnail_prewarm, state];
                    cfg.layout.focused_thumbnail_prewarm = state == 1;
                }
                ControlField::ShowAppNameInCards => {
                    let state: isize = msg_send![u.show_app_name_in_cards, state];
                    cfg.layout.show_app_name_in_cards = state == 1;
                }
                ControlField::CardTextSize => {
                    let val: isize = msg_send![u.card_text_size, integerValue];
                    cfg.layout.card_text_size = val as f64;
                }
                ControlField::StatusBarTextSize => {
                    let val: isize = msg_send![u.status_bar_text_size, integerValue];
                    cfg.fonts.status_bar_size = val as f64;
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
                ControlField::CornerRadius => {
                    // 圆角是数字文本框,走 NSControlText 通知路径,不应出现在这里。
                    // Corner radius is a numeric text field on the notification path; it
                    // should never arrive via target/action.
                    log_debug!("[settings] corner radius via unexpected action path ignored");
                }
                ControlField::Modifier => {
                    let idx: isize = msg_send![u.modifier, indexOfSelectedItem];
                    cfg.keyboard.modifier = if idx == 1 { "command" } else { "option" }.into();
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
                | ControlField::MappingEnabled => {
                    // 这些字段已在函数入口分流到 profile 通道。
                    // These fields are routed to the profile channel at the top.
                    log_debug!("[settings] mouse field via unexpected action path ignored");
                }
                ControlField::ClipboardEnabled => {
                    let state: isize = msg_send![u.clipboard_enabled, state];
                    cfg.clipboard.enabled = state == 1;
                }
                ControlField::ClipboardPersist => {
                    let state: isize = msg_send![u.clipboard_persist, state];
                    cfg.clipboard.persist = state == 1;
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
                ControlField::ClipboardMaxEntries => {
                    // 数字文本框走 NSControlText 通知路径。
                    // Numeric text fields ride the NSControlText notification path.
                    log_debug!("[settings] numeric field via unexpected action path ignored");
                }
                ControlField::ClipboardPinFollow => {
                    let idx: isize = msg_send![u.clipboard_pin_follow, indexOfSelectedItem];
                    cfg.clipboard.pin_follow_selection = idx != 1;
                }
                ControlField::WindowControlEnabled => {
                    let state: isize = msg_send![u.window_control_enabled, state];
                    cfg.window_control.enabled = state == 1;
                }
                ControlField::WindowControlUp => {
                    let state: isize = msg_send![u.window_control_up, state];
                    cfg.window_control.up = state == 1;
                }
                ControlField::WindowControlDown => {
                    let state: isize = msg_send![u.window_control_down, state];
                    cfg.window_control.down = state == 1;
                }
                ControlField::WindowControlLeft => {
                    let state: isize = msg_send![u.window_control_left, state];
                    cfg.window_control.left = state == 1;
                }
                ControlField::WindowControlRight => {
                    let state: isize = msg_send![u.window_control_right, state];
                    cfg.window_control.right = state == 1;
                }
                ControlField::WindowControlDisplayUp => {
                    let state: isize = msg_send![u.window_control_display_up, state];
                    cfg.window_control.display_up = state == 1;
                }
                ControlField::WindowControlDisplayDown => {
                    let state: isize = msg_send![u.window_control_display_down, state];
                    cfg.window_control.display_down = state == 1;
                }
                ControlField::WindowControlDisplayLeft => {
                    let state: isize = msg_send![u.window_control_display_left, state];
                    cfg.window_control.display_left = state == 1;
                }
                ControlField::WindowControlDisplayRight => {
                    let state: isize = msg_send![u.window_control_display_right, state];
                    cfg.window_control.display_right = state == 1;
                }
                ControlField::QuickActionsEnabled => {
                    let state: isize = msg_send![u.quick_actions_enabled, state];
                    cfg.quick_actions.enabled = state == 1;
                }
                ControlField::QuickActionOpenSettings => {
                    let state: isize = msg_send![u.quick_actions_open_settings, state];
                    cfg.quick_actions.open_settings = state == 1;
                }
                ControlField::QuickActionOpenFinder => {
                    let state: isize = msg_send![u.quick_actions_open_finder, state];
                    cfg.quick_actions.open_finder = state == 1;
                }
                ControlField::QuickActionShowDesktop => {
                    let state: isize = msg_send![u.quick_actions_show_desktop, state];
                    cfg.quick_actions.show_desktop = state == 1;
                }
                ControlField::QuickActionLockScreen => {
                    let state: isize = msg_send![u.quick_actions_lock_screen, state];
                    cfg.quick_actions.lock_screen = state == 1;
                }
                ControlField::QuickActionLocatePointer => {
                    let state: isize = msg_send![u.quick_actions_locate_pointer, state];
                    cfg.quick_actions.locate_pointer = state == 1;
                }
                ControlField::UpdateAutoCheck => {
                    let state: isize = msg_send![u.update_auto_check, state];
                    cfg.updates.automatically_check = state == 1;
                }
                ControlField::UpdateAutoDownload => {
                    let state: isize = msg_send![u.update_auto_download, state];
                    cfg.updates.automatically_download = state == 1;
                }
            }
        }
    });
    if let Ok(mut w) = CONFIG.write() {
        *w = cfg.clone();
    }
    schedule_config_persist();
    apply_config_change(&old_cfg, &cfg, ConfigChangeSource::Settings);
    if matches!(field, ControlField::GlassStyle | ControlField::GlassTint) {
        apply_glass_preview();
    }
}

/// 鼠标页 per-device 字段:读控件 → 写选中设备 profile(无档则创建)→ 落盘 + 副作用。
/// Mouse-page per-device fields: read the control → write the selected device's profile
/// (created if absent) → persist + side effects.
pub(super) unsafe fn apply_mouse_profile_field(field: ControlField) {
    with_settings_ui(|ui| {
        let Some(u) = ui.as_mut() else {
            return;
        };
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
                let is_line = mode == "line";
                // Line 模式才读滑块值;Default 模式保留已有行数。
                // Read the slider only in Line mode; Default keeps the existing line count.
                let lc: Option<isize> = if is_line {
                    Some(msg_send![u.line_count, integerValue])
                } else {
                    None
                };
                write_selected_profile(&mut cfg, move |p| {
                    p.scroll_mode = Some(mode);
                    // 仅 Line 模式写回行数;Default 保留已有值。
                    // Write the line count only in Line mode; Default keeps the existing value.
                    if let Some(lc) = lc {
                        p.line_count = Some(lc.clamp(1, 10) as u32);
                    }
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
                // 只改这一个字段:另一字段(跟踪速度)原样保留,不能被整段覆盖掉。
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
                // 夹到 0..=10、取 2 位小数,非有限值退兜底(见 pointer_accel_from_slider)。
                // Clamped to 0..=10 with 2 decimals; a non-finite value falls back (see
                // pointer_accel_from_slider).
                let value = pointer_accel_from_slider(raw);
                write_selected_profile(&mut cfg, move |p| {
                    let mut ptr = p.pointer.take().unwrap_or_default();
                    ptr.acceleration = Some(value);
                    p.pointer = Some(ptr);
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
        if let Ok(mut w) = CONFIG.write() {
            *w = cfg.clone();
        }
        schedule_config_persist();
        apply_config_change(&old_cfg, &cfg, ConfigChangeSource::Settings);
        if field == ControlField::ScrollMode {
            // 滚动模式切换后,行数滑块显示当前生效值并刷新条件显隐。
            // After a mode switch the line-count slider shows the effective value and the
            // conditional row visibility refreshes.
            let cfg_now = CONFIG.read().unwrap().clone();
            let shown = resolve_selected_from(&cfg_now).line_count;
            let _: () = msg_send![u.line_count, setIntegerValue: shown as isize];
            set_field(u.line_count_value_label, shown);
            update_mode_dependent_visibility(u);
        }
    });
}

/// 把选中设备的 profile 交给回调修改(不存在则创建一个)。
/// Hand the selected device's profile to the callback (creating one when absent).
fn write_selected_profile(cfg: &mut Config, f: impl FnOnce(&mut MouseProfile)) {
    let dev = current_selected_device();
    let idx = find_profile_index(cfg, dev);
    let idx = match idx {
        Some(i) => i,
        None => {
            // 若 profile 不存在,新建一个并插入。
            // If the profile doesn't exist, create and insert one.
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
    f(&mut cfg.mouse.profiles[idx]);
}

/// 数字文本框输入中(NSControlTextDidChange):值合法才应用,非法值保留内存配置不动,
/// 磁盘写入走防抖。允许输入过程中的临时非法状态。
/// While typing in a numeric text field (NSControlTextDidChange): apply only when the value
/// is valid; invalid input leaves the in-memory config untouched and persistence goes
/// through the debounce. Transient invalid states are allowed while typing.
pub(crate) extern "C" fn on_control_text_did_change(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut c_void,
) {
    unsafe {
        let obj: *mut AnyObject = msg_send![notification as *mut AnyObject, object];
        let field = match text_field_of(obj) {
            Some(f) => f,
            None => return,
        };
        let Some((value, raw)) = parse_text_field(field) else {
            return;
        };
        let _ = raw;
        write_text_field_config(field, value, false);
        schedule_config_persist();
        log_debug!(
            "[settings] numeric field {:?} applied (debounced persist)",
            field
        );
    }
}

/// 数字文本框失焦 / 回车(NSControlTextDidEndEditing):立即提交并立即落盘;
/// 仍为非法值时把显示恢复为内存配置值。
/// On blur / Enter (NSControlTextDidEndEditing): commit immediately and persist now; when
/// still invalid, restore the displayed value from the in-memory config.
pub(crate) extern "C" fn on_control_text_did_end_editing(
    _self: *mut c_void,
    _cmd: Sel,
    notification: *mut c_void,
) {
    unsafe {
        let obj: *mut AnyObject = msg_send![notification as *mut AnyObject, object];
        let field = match text_field_of(obj) {
            Some(f) => f,
            None => return,
        };
        match parse_text_field(field) {
            Some((value, _)) => {
                write_text_field_config(field, value, true);
                persist_config_now();
                log_debug!(
                    "[settings] numeric field {:?} committed on end editing",
                    field
                );
            }
            None => {
                // 非法值不提交,把输入框恢复为最近一次生效的配置值。
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
}

/// 数字文本框标识(与 ControlField 分离:文本框走通知回调而非 target/action)。
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

/// 解析数字文本框:返回 (归一化后的数值, 原始文本)。非法返回 None。
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

enum TextFieldValue {
    F64(f64),
    U32(u32),
}

/// 把合法的数字值写入内存 CONFIG。
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

/// 根据 enable_mouse switch 状态,冻结或解冻其下方的所有鼠标控件。
/// 未启用时控件灰显且不可交互(AppKit 自动处理灰显),避免用户修改无效配置。
///
/// Freeze or unfreeze all mouse controls below the enable_mouse switch based on its state.
/// When disabled, controls are greyed out and non-interactive (AppKit handles greying), preventing
/// users from editing config that won't take effect.
pub(super) unsafe fn update_mouse_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.enable_mouse, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_mouse_disabled");
    let device_available = !DEVICE_POPUP_KEYS.lock().unwrap().is_empty();
    // 无设备时下拉框始终禁用;其余控件仍由总开关控制。
    // Keep the device popup disabled when no device is connected; the remaining controls follow
    // the master switch.
    if on {
        SettingsRow::set_enabled(ui.device_indicator, device_available);
    } else {
        SettingsRow::set_enabled_with_tooltip(ui.device_indicator, false, &tooltip);
    }
    for &ctrl in &[
        ui.scroll_mode,
        ui.line_count,
        ui.reverse_scroll,
        ui.disable_pointer_accel,
        ui.pointer_accel_slider,
        ui.pointer_accel_value_label,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
    update_mapping_controls_enabled(ui);
}

/// 根据应用切换器总开关状态,冻结其下方的窗口与键盘选项。
/// Freeze the window and keyboard options below the app-switcher master switch.
pub(super) unsafe fn update_windows_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.windows_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_windows_disabled");
    for &ctrl in &[
        ui.show_minimized,
        ui.thumbnails_enabled,
        ui.focused_thumbnail_prewarm,
        ui.show_app_name_in_cards,
        ui.card_text_size,
        ui.card_text_size_value_label,
        ui.status_bar_text_size,
        ui.status_bar_text_size_value_label,
        ui.overlay_position,
        ui.activation_mode,
        ui.corner_radius,
        ui.modifier,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// 根据剪贴板历史总开关状态,冻结其下方的历史选项。
/// Freeze the clipboard-history options below the clipboard master switch.
pub(super) unsafe fn update_clipboard_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.clipboard_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_clipboard_disabled");
    // "同时删除系统剪贴板中对应条目"也在列表里:它只受总开关影响(是否显示由"粘贴后删除条目"
    // 决定,见 update_clipboard_delete_dependent_visibility),所以不再需要按后者叠加置灰。
    // "Clear the matching system-pasteboard entry" is in this list too: only the master switch
    // greys it out (whether it shows at all is decided by "delete entry after paste", see
    // update_clipboard_delete_dependent_visibility), so it no longer stacks a second condition.
    for &ctrl in &[
        ui.clipboard_pin_follow,
        ui.clipboard_persist,
        ui.clipboard_show_source_app,
        ui.clipboard_move_used_to_top,
        ui.clipboard_delete_after_paste,
        ui.clipboard_clear_system_pasteboard_after_paste,
        ui.clipboard_max_entries,
        ui.clipboard_auto_expire_days,
        ui.clipboard_auto_expire_days_value_label,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// 根据窗口控制总开关状态,冻结其下方的八个快捷键开关。
/// Freeze the eight shortcut switches below the window-control master switch.
pub(super) unsafe fn update_window_control_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.window_control_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_window_control_disabled");
    for &ctrl in &[
        ui.window_control_up,
        ui.window_control_down,
        ui.window_control_left,
        ui.window_control_right,
        ui.window_control_display_up,
        ui.window_control_display_down,
        ui.window_control_display_left,
        ui.window_control_display_right,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// 滑块数值的显示格式(2 位小数,与只读数值 label 一致)。
/// The slider value's display format (2 decimals, matching the read-only value label).
pub(super) fn pointer_accel_display(value: f64) -> String {
    format!("{value:.2}")
}

/// 滑杆原始值 -> 配置值:夹到 0..=10 并保留 2 位小数;非有限值退兜底。
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
    // 保留 2 位小数(与只读数值 label 的显示一致,避免把浮点噪声写进配置)。
    // Keep 2 decimals (matching the read-only value label, and keeping floating-point noise out
    // of the config).
    (clamped * 100.0).round() / 100.0
}

/// 根据"禁用指针加速(线性跟踪)"开关状态刷新跟踪速度行的条件显隐:
/// - 开关打开(线性跟踪):显示"跟踪速度"行
/// - 开关关闭:隐藏该行,下方分组上收
///
/// 该行的值只在开关打开时生效:线性跟踪下 HIDPointerAcceleration 才是跟踪速度,开关关闭时
/// 它是加速曲线的强度,含义不同(见 mouse/pointer.rs 模块注释),所以不共用、也不在关闭时显示。
///
/// Refresh the conditional visibility of the tracking-speed row from the disable-acceleration
/// switch:
/// - switch on (linear tracking): the "Tracking speed" row is shown
/// - switch off: the row is hidden and the sections below move up
///
/// The value only takes effect while the switch is on: HIDPointerAcceleration is the tracking
/// speed under linear tracking and the acceleration curve's strength otherwise, a different
/// meaning (see the module comment in mouse/pointer.rs) -- so it is neither shared nor shown
/// while off.
unsafe fn update_pointer_accel_visibility(ui: &SettingsUi) {
    let state: isize = msg_send![ui.disable_pointer_accel, state];
    // 整块收放(卡片底边上收、下方分组上移、分割线一起藏)由 CollapsibleRows 负责。
    // CollapsibleRows owns the whole collapse (card bottom edge, sections below, divider).
    ui.pointer_accel_block.set_visible(state == 1);
}

/// 根据当前滚动模式(Default/Line)刷新"行数"行的条件显隐:
/// - Line:显示"每 tick 行数"行
/// - Default:隐藏
///
/// 由 load_settings_values 与 handle_scroll_mode_changed 调用。
///
/// Refresh the conditional visibility of the "lines per tick" row based on the current scroll mode
/// (Default/Line):
/// - Line: the "lines per tick" row is shown
/// - Default: hidden
///
/// Called by load_settings_values and handle_scroll_mode_changed.
unsafe fn update_mode_dependent_visibility(ui: &SettingsUi) {
    let idx: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
    let mode = SCROLL_MODE_VALUES
        .get(idx as usize)
        .copied()
        .unwrap_or("default");
    // 只有 Line 模式显示行数滑块(Default 不显示);整块收放由 CollapsibleRows 负责。
    // Only Line mode shows the line-count slider (hidden on Default); CollapsibleRows owns the
    // collapse (card bottom edge, sections below, divider).
    ui.line_count_block.set_visible(mode == "line");
}

/// 根据窗口显示模式刷新"仅缩略图"两行的条件显隐:
/// - 图标和缩略图(index 1):显示前台预热与"缩略图上显示应用名"
/// - 仅图标(index 0):两行一起隐藏(没有缩略图时两者都无意义)
///
/// Refresh the visibility of the thumbnail-only pair from the window display mode:
/// - icons and thumbnails (index 1): show focused prewarm and "app name on thumbnails"
/// - icons only (index 0): hide both (neither means anything without thumbnails)
unsafe fn update_display_mode_dependent_visibility(ui: &SettingsUi) {
    let idx: isize = msg_send![ui.thumbnails_enabled, indexOfSelectedItem];
    // 下拉 index 0 = 仅图标, 1 = 图标和缩略图(与配置里的 thumbnails_enabled 布尔值同义)。
    // Popup index 0 = icons only, 1 = icons and thumbnails (same as the layout.thumbnails_enabled
    // boolean).
    ui.thumbnail_only_block.set_visible(idx == 1);
}

/// "同时删除系统剪贴板中对应条目"只在"粘贴后删除条目"打开时出现(它是后者的子项)。
/// Show the "clear the matching system-pasteboard entry" row only while "delete entry after
/// paste" is on (it is that switch's child option).
unsafe fn update_clipboard_delete_dependent_visibility(ui: &SettingsUi) {
    let state: isize = msg_send![ui.clipboard_delete_after_paste, state];
    ui.clipboard_delete_block.set_visible(state == 1);
}

/// 条件行区块一起重算。幂等(状态由实时 frame 推出),可在窗口显示前后各调一次。
/// Recompute all conditional row blocks. Idempotent (the state comes from the live frames),
/// so it is safe both before and after the window is on screen.
pub(super) unsafe fn update_conditional_rows(ui: &SettingsUi) {
    update_mode_dependent_visibility(ui);
    update_pointer_accel_visibility(ui);
    update_display_mode_dependent_visibility(ui);
    update_clipboard_delete_dependent_visibility(ui);
}

/// enable_mouse switch toggle 回调:即时应用 + 冻结/解冻下方控件。
/// Callback when the enable_mouse switch is toggled: apply immediately, then freeze/unfreeze
/// the controls below.
pub(crate) extern "C" fn handle_enable_mouse_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    apply_control_field(ControlField::MouseEnabled);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                update_mouse_controls_enabled(u);
            }
        });
    }
}

/// 应用切换器总开关回调:即时应用 + 冻结/解冻下方窗口与键盘选项。
/// Callback for the app-switcher master switch: apply immediately, then freeze/unfreeze the
/// window and keyboard options below.
pub(crate) extern "C" fn handle_windows_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    apply_control_field(ControlField::WindowsEnabled);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                update_windows_controls_enabled(u);
            }
        });
    }
}

/// 剪贴板历史总开关回调:即时应用 + 冻结/解冻下方历史选项。
/// Callback for the clipboard-history master switch: apply immediately, then freeze/unfreeze
/// the history options below.
pub(crate) extern "C" fn handle_clipboard_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    apply_control_field(ControlField::ClipboardEnabled);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                update_clipboard_controls_enabled(u);
            }
        });
    }
}

/// 窗口控制总开关回调:即时应用 + 冻结/解冻下方八个快捷键开关。
/// Callback for the window-control master switch: apply immediately, then freeze/unfreeze its
/// eight shortcut switches.
pub(crate) extern "C" fn handle_window_control_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    apply_control_field(ControlField::WindowControlEnabled);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                update_window_control_controls_enabled(u);
            }
        });
    }
}

/// 快捷操作总开关回调:即时应用 + 冻结/解冻下方五个动作开关。
/// Callback for the quick-actions master switch: apply immediately, then freeze/unfreeze the
/// five action switches below.
pub(crate) extern "C" fn handle_quick_actions_enabled_toggle(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    apply_control_field(ControlField::QuickActionsEnabled);
    unsafe {
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                update_quick_actions_controls_enabled(u);
            }
        });
    }
}

/// 根据快捷操作总开关状态,冻结/解冻下方五个动作开关。
/// Freeze/unfreeze the five action switches below the quick-actions master switch.
pub(super) unsafe fn update_quick_actions_controls_enabled(ui: &SettingsUi) {
    let state: isize = msg_send![ui.quick_actions_enabled, state];
    let on = state == 1;
    let tooltip = t("settings.tooltip_quick_actions_disabled");
    for &ctrl in &[
        ui.quick_actions_open_settings,
        ui.quick_actions_open_finder,
        ui.quick_actions_show_desktop,
        ui.quick_actions_lock_screen,
        ui.quick_actions_locate_pointer,
    ] {
        SettingsRow::set_enabled_with_tooltip(ctrl, on, &tooltip);
    }
}

/// Refresh top-level service switches when they are changed from the status-bar menu.
/// 当状态栏菜单修改功能大类开关时，同步设置页中的总开关状态。
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
                (u.window_control_enabled, cfg.window_control.enabled),
                (u.quick_actions_enabled, cfg.quick_actions.enabled),
            ] {
                let _: () = msg_send![ctrl, setState: if value { 1isize } else { 0isize }];
            }
            update_windows_controls_enabled(u);
            update_mouse_controls_enabled(u);
            update_clipboard_controls_enabled(u);
            update_window_control_controls_enabled(u);
            update_quick_actions_controls_enabled(u);
        });
    }
}

/// Refresh the switcher controls when an external surface changes them.
///
/// This intentionally updates the existing controls in place instead of rebuilding the settings
/// window. That preserves unsaved text edits and, like the appearance refresh path, never brings
/// a hidden settings window to the foreground.
///
/// 当其他界面修改切换器设置时刷新设置页控件。
///
/// 这里刻意只原位更新现有控件,不重建设置窗口:这样不会丢失尚未提交的文本编辑,也不会像
/// 打开设置那样把隐藏的设置窗口带到前台。
pub(crate) fn refresh_switcher_controls_from_config() {
    let cfg = CONFIG.read().unwrap().clone();
    unsafe {
        with_settings_ui(|ui| {
            let Some(u) = ui.as_mut() else {
                return;
            };
            let visible: bool = msg_send![u.window, isVisible];
            if !visible {
                return;
            }

            let modifier_idx: isize = if cfg.keyboard.modifier == "command" {
                1
            } else {
                0
            };
            let _: () = msg_send![u.modifier, selectItemAtIndex: modifier_idx];

            let thumbnail_idx: isize = if cfg.layout.thumbnails_enabled { 1 } else { 0 };
            let _: () = msg_send![u.thumbnails_enabled, selectItemAtIndex: thumbnail_idx];
            let prewarm_state = if cfg.layout.focused_thumbnail_prewarm {
                1isize
            } else {
                0isize
            };
            let _: () = msg_send![u.focused_thumbnail_prewarm, setState: prewarm_state];
            // 显示模式可能刚变过:仅缩略图的两行跟着重算显隐。
            // The display mode may have just changed: recompute the thumbnail-only pair.
            update_display_mode_dependent_visibility(u);
        });
    }
}

/// 设备下拉框的项与 DeviceKey 的映射(与 popup items 一一对应),供 handle_device_changed
/// 按 indexOfSelectedItem 反查。每次 rebuild_device_popup 重建。
/// 只有具体设备项,无"所有鼠标"通配项。
///
/// Mapping from popup-item index to DeviceKey (1:1 with popup items), used by
/// handle_device_changed to look up the selected device by indexOfSelectedItem. Rebuilt each time
/// rebuild_device_popup runs. Contains only concrete devices; no "All Mice" wildcard entry.
static DEVICE_POPUP_KEYS: Mutex<Vec<crate::mouse::device::DeviceKey>> = Mutex::new(Vec::new());

/// 基于当前已连接设备列表初始化/校准 SELECTED_DEVICE。
/// 必须在 resolve_selected() 之前调用,保证 resolve 拿到的是有效设备:
/// - 未初始化(首次打开设置)-> 选中第一个设备(若有)
/// - 已初始化但所选设备已被拔出 -> 回退到第一个设备
/// - 所选设备仍在列表 -> 保持
///
/// 无设备连接时清空选中(编辑"所有鼠标"基础层)。
/// 每次打开设置都重新校准,天然处理热插拔(设备增减)。
///
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
        // 无设备连接:清空选中状态(编辑"所有鼠标"基础层)。
        // No device connected: clear the selection (edits the "All Mice" base layer).
        *SELECTED_DEVICE.lock().unwrap() = Some(None);
        return;
    }

    // 当前设备仍在列表 -> 保持;否则(未初始化或被拔出)回退到第一个设备。
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

/// 重建设备下拉框的选项:仅各已连接设备(无"所有鼠标"通配项)。
/// 只负责 UI(items + 选中项);SELECTED_DEVICE 的状态校准由 ensure_selected_device 负责。
/// 由 load_settings_values 调用(每次打开设置时刷新,反映热插拔)。
///
/// Rebuild the device popup's items: only each connected device (no "All Mice" wildcard entry).
/// UI only (items + selection); SELECTED_DEVICE state calibration is handled by
/// ensure_selected_device. Called by load_settings_values (refreshed on each settings open to
/// reflect hot-plug changes).
pub(super) unsafe fn rebuild_device_popup(ui: &SettingsUi) {
    let connected = crate::mouse::device::connected_devices();
    let cur = current_selected_device();

    // 构建下拉项与 key 映射:仅设备。
    // Build the popup items and the key mapping: devices only.
    let mut items: Vec<String> = Vec::new();
    let mut keys: Vec<crate::mouse::device::DeviceKey> = Vec::new();
    for d in &connected {
        items.push(format!(
            "{} ({:#x}:{:#x})",
            d.name, d.vendor_id, d.product_id
        ));
        keys.push((d.vendor_id, d.product_id));
    }

    // 清空旧项,填入新项。
    // Clear old items and fill in the new ones.
    let _: () = msg_send![ui.device_indicator, removeAllItems];
    for s in &items {
        let ns = make_nsstring(s);
        let _: () = msg_send![ui.device_indicator, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }

    // 选中当前设备对应的项;若已不在列表,选中第一个。
    // Select the item matching the current device; if it's gone, select the first.
    let sel_idx = cur
        .and_then(|c| keys.iter().position(|k| *k == c))
        .unwrap_or(0);
    if !keys.is_empty() {
        let _: () = msg_send![ui.device_indicator, selectItemAtIndex: sel_idx as isize];
    } else {
        // 空列表时保留一个明确的不可选提示,避免空白下拉框看起来像加载失败。
        // Keep one explicit, non-selectable status item when the list is empty so the popup
        // does not look like a failed or incomplete load.
        let ns = make_nsstring(&t("settings.no_device_detected"));
        let _: () = msg_send![ui.device_indicator, addItemWithTitle: ns];
        CFRelease(ns as *const c_void);
    }
    let _: () = msg_send![ui.device_indicator, setEnabled: !keys.is_empty()];

    *DEVICE_POPUP_KEYS.lock().unwrap() = keys;
}

/// 设置窗口开着时即时刷新设备下拉框(由插拔事件经主线程调用)。
/// 设备列表是外部实时状态(硬件插拔),不属于 OK/Cancel 门控范围——重连后应立即显示,
/// 无需点确定或重开设置。重建下拉用内存态 SELECTED_DEVICE 恢复选中,不会重置用户
/// 未保存的选择。窗口未打开时无操作(下次打开时 load_settings_values 仍会重建)。
///
/// Refresh the device popup live while the settings window is open (called on the main
/// thread from device plug/unplug events). The device list is external live state (hardware
/// attach/detach), not part of the OK/Cancel-gated preferences -- a reconnect should show
/// immediately without OK or reopening. The rebuild restores the selection from the in-memory
/// SELECTED_DEVICE, so unsaved choices survive. No-op when the window isn't open (it is
/// rebuilt on next open via load_settings_values anyway).
pub(crate) fn refresh_device_popup_if_open() {
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

/// 设备下拉框切换回调:更新 SELECTED_DEVICE 并即时刷新其余控件为新设备的有效值。
/// Device-popup selection-changed callback: update SELECTED_DEVICE and immediately refresh the
/// other controls with the newly-selected device's effective values.
pub(crate) extern "C" fn handle_device_changed(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let popup = sender as *mut AnyObject;
    let idx: isize = unsafe { msg_send![popup, indexOfSelectedItem] };
    // DEVICE_POPUP_KEYS: Vec<DeviceKey>;取选中项对应的 key(均为具体设备,无通配项)。
    // DEVICE_POPUP_KEYS: Vec<DeviceKey>; get the key for the selected item (all concrete
    // devices; no wildcard entry).
    let new_dev = DEVICE_POPUP_KEYS.lock().unwrap().get(idx as usize).copied();
    *SELECTED_DEVICE.lock().unwrap() = Some(new_dev);
    // 只刷新鼠标页的 per-device 控件,不能走完整 load_settings_from——那会把
    // enable_mouse switch 重置为已保存的 cfg.mouse.enabled,冲掉用户刚勾选
    // 但尚未点 OK 的修改(启用鼠标控制是全局设置,切换设备不应动它)。
    // Only refresh the mouse page's per-device controls -- a full load_settings_from would
    // reset the enable_mouse switch to the saved cfg.mouse.enabled, wiping the user's
    // unsaved toggle (enable mouse control is a global setting; device switches must not
    // touch it).
    let cfg = CONFIG.read().unwrap().clone();
    let resolved = resolve_selected_from(&cfg);
    unsafe {
        with_settings_ui(|ui_guard| {
            if let Some(u) = ui_guard.as_mut() {
                fill_mouse_device_controls(u, &resolved);
                // enable_mouse 勾选状态保持用户当前值;只重算冻结与条件显隐。
                // Keep the user's current enable_mouse state; only recompute freeze + visibility.
                update_mouse_controls_enabled(u);
                update_mode_dependent_visibility(u);
                update_pointer_accel_visibility(u);
                // 设备切换:映射编辑态换成新设备的专属 mappings 并重渲染。
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
}

// 侧边栏点击回调:读 sender 的 tag,切换到对应页。
// Sidebar click callback: read the sender's tag and switch to that page.

pub(crate) extern "C" fn on_sidebar_select(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    // Navigating away also cancels an unfinished destructive confirmation.
    // 切换到其他页面时同时取消尚未确认的危险操作(整页与整应用两套确认卡片)。
    collapse_restore_confirmations(true);
    let btn = sender as *mut AnyObject;
    let tag: isize = unsafe { msg_send![btn, tag] };
    select_sidebar(tag as usize);
    unsafe {
        // Keep an invisible origin at the clicked row so the next adjacent hover can glide from it.
        // 点击后保留当前行的不可见起点，让下一次相邻悬停可以从这里滑过去。
        widgets::prime_sidebar_hover_after_selection(btn);
    }
}

// ========== 导出日志 / export logs ==========

/// 设置「导出日志」按钮回调:把当前活动日志文件复制到用户经 NSSavePanel 选择的位置。
/// 读取得到的是此刻的快照,后台 writer 继续往原文件追加,互不影响。
///
/// The settings "export logs" button: copy the active log file to a user-chosen
/// NSSavePanel destination. The read yields a point-in-time snapshot; the background
/// writer keeps appending to the original file, so the two never interfere.
pub(crate) extern "C" fn handle_export_logs(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    let Some(source) = crate::logger::active_log_path() else {
        super::window::show_alert(
            &t("settings.export_failed_title"),
            &t("settings.export_failed_no_source"),
        );
        return;
    };
    // 用户取消保存面板 = 静默无操作 / a cancelled save panel is a silent no-op
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
}

/// 导出文件名建议:oh-my-tab-YYYYMMDD-HHMMSS.log。导出的是某一刻的快照,时间戳
/// 让多次导出不互相覆盖。
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
