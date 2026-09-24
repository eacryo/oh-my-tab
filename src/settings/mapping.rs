//! Button mappings: mapping-row rendering, side-button recording (dedicated CGEventTap thread), and the edit panel.

use super::*;

/// Render the selected device's button-mapping rows into the scroll container (called after
/// recording / deletion / device switch). Old rows are removed first, then rebuilt sorted by
/// button number; mapping_doc is flipped, so rows stack top-down.
/// Flush MAPPING_EDITS into the selected device's own profile (created if absent) and
/// schedule a debounced persist. Live-apply mode: mapping edits no longer wait for a
/// bottom-right confirm; they hit the in-memory config as soon as they are confirmed/deleted.
pub(super) fn commit_mapping_edits() {
    let edits = MAPPING_EDITS.lock().unwrap().clone();
    let mut cfg = crate::config::CONFIG.read().unwrap().clone();
    let idx = super::selected_device_profile_index(&mut cfg);
    cfg.mouse.profiles[idx].button_mappings = edits;
    if let Ok(mut w) = crate::config::CONFIG.write() {
        *w = cfg;
    }
    crate::config::schedule_config_persist();
    // Config changed: invalidate the per-device resolve cache (next resolve re-merges).
    crate::mouse::resolve::invalidate_cache();
}

pub(super) fn render_mapping_rows() {
    unsafe {
        super::with_settings_ui(|ui| {
            let Some(u) = ui.as_mut() else { return };
            render_mapping_rows_locked(u);
        });
    }
}

/// Freeze every editable mapping control based on both the mouse and mappings master switches.
pub(super) unsafe fn update_mapping_controls_enabled(u: &SettingsUi) {
    let mouse_state: isize = msg_send![u.enable_mouse, state];
    let mapping_state: isize = msg_send![u.mapping_enabled, state];
    let mouse_on = mouse_state == 1;
    let mappings_on = mouse_on && mapping_state == 1;
    let tooltip = if mouse_on {
        t("settings.tooltip_mapping_disabled")
    } else {
        t("settings.tooltip_mouse_disabled")
    };
    SettingsRow::set_enabled_with_tooltip(u.mapping_enabled, mouse_on, &tooltip);
    let _: () = msg_send![u.add_mapping_button, setEnabled: mappings_on];
    for row in &u.mapping_rows {
        for ctrl in row.interactive_views() {
            if !ctrl.is_null() {
                SettingsRow::set_view_enabled_with_tooltip(ctrl, mappings_on, Some(&tooltip));
            }
        }
    }
}

/// Locked variant: used when the caller already holds the SETTINGS_UI lock
/// (load_settings_from / handle_device_changed), avoiding a self-deadlock on the same
/// non-reentrant Mutex.
pub(super) unsafe fn render_mapping_rows_locked(u: &mut SettingsUi) {
    unsafe {
        // removeFromSuperview already drops the superview's reference; the alloc +1 was
        // balanced by release_obj right after addSubview. Re-releasing here would double-free
        // (EXC_BAD_ACCESS).
        let stale = u.mapping_rows.len();
        for row in u.mapping_rows.drain(..) {
            // Unregister BEFORE destroying. The disabled-hint/tracking registries are keyed by the
            // raw view address and hold no ownership, so a missed unregister leaves a dangling key:
            // the next settings click then goes sendEvent -> handle_mouse_down -> message to freed
            // memory and traps with EXC_BREAKPOINT (the 2026-09-15 crash report is this path).
            for ctrl in row.interactive_views() {
                SettingsRow::forget(ctrl);
            }
            let _: () = msg_send![row.label, removeFromSuperview];
            let _: () = msg_send![row.desc_label, removeFromSuperview];
            let _: () = msg_send![row.action_icon, removeFromSuperview];
            let _: () = msg_send![row.edit, removeFromSuperview];
            let _: () = msg_send![row.delete, removeFromSuperview];
            for cap in row.caps {
                let _: () = msg_send![cap, removeFromSuperview];
            }
            let _: () = msg_send![row.separator, removeFromSuperview];
        }
        let doc = u.mapping_doc;
        // The list shows bound rows plus freshly added unconfigured ones (in-row config
        // from scheme A, but dynamic rows).
        let mut items: Vec<(u32, String)> = MAPPING_EDITS
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(b, d)| b.parse::<u32>().ok().map(|n| (n, d.clone())))
            .collect();

        log_debug!(
            "[mouse] render mappings: removed {} stale rows, {} live entries",
            stale,
            items.len()
        );
        items.sort_by_key(|(b, _)| *b);
        let items_len = items.len();
        let row_h = MAPPING_ROW_H;
        // The card height grows with the row count (top-anchored): it keeps three rows when
        // short and grows downward when long, the page scroll view handles the overflow.
        // Empty-state hint: shown when there are no rows.
        let _: () = msg_send![u.mapping_empty, setHidden: !items.is_empty()];
        // Resize height only, keeping the initial width: setFrameSize(0.0, doc_h) used to
        // zero the width, and a zero-width document view fails hit-testing -- the delete
        // buttons became unclickable.
        // Flipped: y=0 is the top; rows stack down from the top.
        let mouse_on = {
            let st: isize = msg_send![u.enable_mouse, state];
            st == 1
        };
        let mappings_on = {
            let st: isize = msg_send![u.mapping_enabled, state];
            mouse_on && st == 1
        };
        let _: () = msg_send![u.mapping_enabled, setEnabled: mouse_on];
        // The add button greys out too (no new mappings while off).
        let _: () = msg_send![u.add_mapping_button, setEnabled: mappings_on];
        // Rows sit inside the nested table panel: a left content inset and right-aligned actions.
        let row_x0 = MAPPING_PANEL_X + MAPPING_CELL_X;
        let card_frame: NSRect = msg_send![doc, frame];
        let card_w = card_frame.size.width;
        let row_right = card_w - MAPPING_PANEL_X - MAPPING_CELL_X;
        let btn_w = 60.0;
        let btn_gap = 6.0;
        let ed_x = row_right - (btn_w * 2.0 + btn_gap);
        let del_x = ed_x + btn_w + btn_gap;
        let btn_h = 27.0;
        let desc_x = row_x0 + 74.0;
        let desc_text_x = desc_x + 28.0;
        // Rows start below the header band (MAPPING_PANEL_TOP + MAPPING_HEADER_H).
        let mut y = MAPPING_PANEL_TOP + MAPPING_HEADER_H;
        let target = MENU_TARGET.lock().unwrap().unwrap().0;
        for (btn, desc) in items {
            // Action-type index: default / none / system action / shortcut (Key Press).
            let (action_idx, is_key) = match crate::mouse::shortcut::parse_binding(&desc) {
                Ok(crate::mouse::shortcut::Binding::Key(_)) => (2, true),
                Ok(crate::mouse::shortcut::Binding::System(_)) => {
                    // System-action names map to popup indices (3..=6).
                    let idx = crate::mouse::shortcut::SYSTEM_ACTIONS
                        .iter()
                        .position(|a| a.eq_ignore_ascii_case(&desc))
                        .map(|i| i + 3)
                        .unwrap_or(0);
                    (idx, false)
                }
                Ok(crate::mouse::shortcut::Binding::Switcher) => (7, false),
                Ok(crate::mouse::shortcut::Binding::None) => (1, false),
                Err(_) => (0, false),
            };
            // The button name.
            // The 13pt text sits toward the TOP of the 28pt field (not vertically centered):
            // shifting the label down 7pt aligns its midline with the buttons' (calibrated).
            let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(row_x0, y + (row_h - 22.0) / 2.0), NSSize::new(70.0, 22.0))];
            set_field(label, 0);
            let _: () = msg_send![label, setBezeled: false];
            let _: () = msg_send![label, setDrawsBackground: false];
            let _: () = msg_send![label, setEditable: false];
            apply_settings_text_role(label, SettingsTextRole::Primary);
            let name_ns = make_nsstring(&button_name(btn));
            let _: () = msg_send![label, setStringValue: name_ns];
            CFRelease(name_ns as *const c_void);
            let _: () = msg_send![label, setEnabled: mappings_on];
            let _: () = msg_send![doc, addSubview: label];
            release_obj(label);
            // The action description: text for system actions/None; keycaps for Key Press.
            let desc_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let desc_label: *mut AnyObject = msg_send![desc_label, initWithFrame: NSRect::new(NSPoint::new(desc_text_x, y + (row_h - 22.0) / 2.0), NSSize::new((ed_x - desc_text_x - 8.0).max(1.0), 22.0))];
            set_field(desc_label, 0);
            let _: () = msg_send![desc_label, setBezeled: false];
            let _: () = msg_send![desc_label, setDrawsBackground: false];
            let _: () = msg_send![desc_label, setEditable: false];
            apply_settings_text_role(desc_label, SettingsTextRole::Primary);
            let _: () = msg_send![desc_label, setEnabled: mappings_on];
            if !is_key && action_idx > 0 {
                // The action-name text for system actions/None (i18n labels).
                let key = MAPPING_ACTION_KEYS
                    .get(action_idx)
                    .copied()
                    .unwrap_or("settings.mapping_action_default");
                let ns = make_nsstring(&t(key));
                let _: () = msg_send![desc_label, setStringValue: ns];
                CFRelease(ns as *const c_void);
            }
            let _: () = msg_send![doc, addSubview: desc_label];
            release_obj(desc_label);
            // Non-key actions reuse the same SF Symbol as the edit-panel popup so the mapping
            // row carries the same visual/action cue. Key Press keeps its keycap pills below.
            let action_icon = if !is_key {
                let icon = SettingsMappingActionIcon::attach(
                    doc,
                    action_idx,
                    NSRect::new(
                        NSPoint::new(
                            desc_x,
                            y + (row_h - SettingsMappingActionIcon::ROW_SIZE) / 2.0,
                        ),
                        NSSize::new(
                            SettingsMappingActionIcon::ROW_SIZE,
                            SettingsMappingActionIcon::ROW_SIZE,
                        ),
                    ),
                );
                // The document view now owns the icon; balance the builder's alloc reference.
                release_obj(icon);
                icon
            } else {
                std::ptr::null_mut()
            };
            // The edit button (opens the edit panel).
            let edit = SettingsButton::action(
                NSRect::new(
                    NSPoint::new(ed_x, y + (row_h - btn_h) / 2.0),
                    NSSize::new(btn_w, btn_h),
                ),
                &t("settings.mapping_edit"),
                target,
                sel!(handleMappingEdit:),
                SettingsButtonRole::Compact,
            );
            let _: () = msg_send![edit, setTag: btn as isize];
            let _: () = msg_send![edit, setEnabled: mappings_on];
            let _: () = msg_send![doc, addSubview: edit];
            release_obj(edit);
            // The delete button (text style, same look as Edit).
            let delete = SettingsButton::action(
                NSRect::new(
                    NSPoint::new(del_x, y + (row_h - btn_h) / 2.0),
                    NSSize::new(btn_w, btn_h),
                ),
                &t("settings.mapping_delete"),
                target,
                sel!(handleDeleteMapping:),
                SettingsButtonRole::Compact,
            );
            let _: () = msg_send![delete, setTag: btn as isize];
            let _: () = msg_send![delete, setEnabled: mappings_on];
            let _: () = msg_send![doc, addSubview: delete];
            release_obj(delete);
            // Keycap pills: one rounded square per modifier symbol + the main key (like
            // real keyboard keycaps).
            let mut caps: Vec<*mut AnyObject> = Vec::new();
            if is_key {
                let key_str = display_shortcut(&desc);
                let cap_size = 20.0;
                let cap_y = y + (row_h - cap_size) / 2.0;
                let mut cap_x = desc_x + 4.0;
                for ch in key_str.chars() {
                    let cap: *mut AnyObject = msg_send![class!(NSTextField), alloc];
                    let cap: *mut AnyObject = msg_send![cap, initWithFrame: NSRect::new(NSPoint::new(cap_x, cap_y), NSSize::new(cap_size, cap_size))];
                    set_field(cap, 0);
                    let _: () = msg_send![cap, setBezeled: false];
                    let _: () = msg_send![cap, setDrawsBackground: false];
                    let _: () = msg_send![cap, setEditable: false];
                    apply_settings_text_role(cap, SettingsTextRole::Primary);
                    let _: () = msg_send![cap, setAlignment: 1isize]; // center
                    let _: () = msg_send![cap, setEnabled: mappings_on];
                    let ch_ns = make_nsstring(&ch.to_string());
                    let _: () = msg_send![cap, setStringValue: ch_ns];
                    CFRelease(ch_ns as *const c_void);
                    // Rounded light-gray backing.
                    let _: () = msg_send![cap, setWantsLayer: true];
                    let cap_layer: *mut AnyObject = msg_send![cap, layer];
                    let _: () = msg_send![cap_layer, setCornerRadius: 4.0f64];
                    let cap_bg: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
                    layer_set_background(cap_layer, ns_color_to_cg(cap_bg));
                    let _: () = msg_send![doc, addSubview: cap];
                    release_obj(cap);
                    caps.push(cap);
                    cap_x += cap_size + 4.0;
                }
            }
            // Row-bottom separator (the last row's line is clipped by the card corner).
            let sep: *mut AnyObject = msg_send![class!(NSView), alloc];
            let sep: *mut AnyObject = msg_send![sep, initWithFrame: NSRect::new(NSPoint::new(row_x0, y + row_h - 1.0), NSSize::new(row_right - row_x0, 1.0))];
            let _: () = msg_send![sep, setWantsLayer: true];
            let sep_layer: *mut AnyObject = msg_send![sep, layer];
            let sep_color: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
            layer_set_background(sep_layer, ns_color_to_cg(sep_color));
            let _: () = msg_send![doc, addSubview: sep];
            release_obj(sep);
            u.mapping_rows.push(MappingRow {
                label,
                desc_label,
                action_icon,
                edit,
                delete,
                separator: sep,
                caps,
            });
            y += row_h;
        }
        // Grow the nested table + card to fit every row. The panel and the add button track the
        // growing table; the page scroll view handles any overflow, so nothing is clipped.
        {
            let card_frame: NSRect = msg_send![doc, frame];
            let card_w = card_frame.size.width;
            let panel_h = (MAPPING_HEADER_H + items_len as f64 * MAPPING_ROW_H)
                .max(MAPPING_HEADER_H + MAPPING_ROW_H * 3.0);
            let card_h = MAPPING_PANEL_TOP
                + panel_h
                + MAPPING_ACTION_TOP
                + MAPPING_ACTION_H
                + MAPPING_CARD_PAD_BOT;
            let card_top = card_frame.origin.y + card_frame.size.height;
            let card_bottom = card_top - card_h;
            // The panel keeps its top-left anchor; its height tracks the rows.
            let _: () = msg_send![u.mapping_panel, setFrameSize: NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, panel_h)];
            let _: () = msg_send![doc, setFrame: NSRect::new(NSPoint::new(card_frame.origin.x, card_bottom), NSSize::new(card_w, card_h))];
            // The add button sits in the action row at the card bottom.
            let _: () = msg_send![u.add_mapping_button, setFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X, MAPPING_PANEL_TOP + panel_h + MAPPING_ACTION_TOP), NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_ACTION_H))];
        }
        // Re-apply component state after rebuilding dynamic rows so labels, cursor, and tooltips
        // match the current mouse/mapping master switches.
        update_mapping_controls_enabled(u);
    }
}

/// Wake the settings callback on the main thread (argument-less variant).
pub(super) fn notify_main(sel: Sel) {
    // Read the Send-safe dispatch handle, not the main-thread-only MENU_TARGET: this function is
    // called **on the recording thread** (finished/cancelled/stage all come from the recording tap
    // callback), and reading MENU_TARGET off-main trips the main-thread assertion in debug builds;
    // the caller is an extern "C" callback, so that panic cannot unwind and aborts the process.
    let target = *crate::MENU_TARGET_DISPATCH.lock().unwrap();
    if let Some(t) = target {
        unsafe {
            let _: () = msg_send![
                t.0,
                performSelectorOnMainThread: sel,
                withObject: std::ptr::null::<AnyObject>(),
                waitUntilDone: false
            ];
        }
    }
}

/// Common teardown for recording finish/cancel: reset the stage and the RECORDING flag,
/// stop the recording thread's RunLoop, and wake the main-thread callback. Called on the
/// recording tap thread.
/// Defensive recording cancel: when the settings window OKs/cancels/closes while a
/// recording is in progress, reset the state so nothing lingers.
pub(crate) fn cancel_recording_from_main() {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        *REC_STAGE.lock().unwrap() = RecStage::Idle;
        *REC_MODS.lock().unwrap() = 0;
        REC_DESC.lock().unwrap().clear();
        REC_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
        crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
        disable_rec_tap();
        if let Some(rl) = *REC_RUNLOOP.0.lock().unwrap() {
            unsafe {
                crate::event_tap::CFRunLoopStop(rl);
            }
        }
    }
}

/// Immediately disable the recording tap (if any). Disabling is synchronous: after
/// CGEventTapEnable(false) the tap receives nothing more.
pub(super) fn disable_rec_tap() {
    if let Some(tap) = *REC_TAP.0.lock().unwrap() {
        unsafe {
            crate::event_tap::CGEventTapEnable(tap, false);
        }
    }
}

pub(super) unsafe fn finish_recording(success: bool) {
    *REC_STAGE.lock().unwrap() = RecStage::Idle;
    // Clear the intermediates on finish/cancel, so nothing leaks into the next session.
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    REC_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
    // Disable the tap before stopping the loop: disabling takes effect immediately,
    // eliminating the exit-window swallowing.
    disable_rec_tap();
    if let Some(rl) = *REC_RUNLOOP.0.lock().unwrap() {
        crate::event_tap::CFRunLoopStop(rl);
    }
    notify_main(if success {
        sel!(handleRecordingFinished:)
    } else {
        sel!(handleRecordingCancelled:)
    });
}

/// Recording tap callback (recording thread): a combo keyDown finishes the recording
/// (bare Esc cancels). The combo input is swallowed; flagsChanged passes through while
/// refreshing the popup's modifier display live.
pub(super) unsafe extern "C" fn recording_tap_callback(
    proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    // The same panic boundary every other event-tap callback uses: a panic cannot unwind through
    // extern "C" and aborts the whole process -- exactly how the recording callback used to crash
    // (see the note in notify_main). The fallback passes the input through rather than swallowing
    // it: missing one swallow beats killing the app.
    crate::callback_guard::event("recording_tap_callback", event, || unsafe {
        recording_tap_callback_inner(proxy, event_type, event, user_info)
    })
}

unsafe fn recording_tap_callback_inner(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    if crate::input_monitor::handle_disabled_event(event_type, "rec") {
        return event;
    }
    if !crate::input_monitor::taps_allowed() {
        return event;
    }
    match event_type {
        25 => {
            // Button number lives in field 3 (same as mouse/event_tap.rs). Only capture/
            // swallow while WaitingButton; a lingering tap after cancel passes everything.
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingButton {
                let btn = CGEventGetIntegerValueField(event, 3) as u32;
                if btn >= 2 {
                    *REC_BUTTON.lock().unwrap() = btn;
                    // Panel trigger recording: the side button completes it; the callback
                    // updates the panel.
                    finish_recording(true);
                    return std::ptr::null_mut();
                }
            }
            event
        }
        12 => {
            // flagsChanged: during WaitingCombo, accumulate modifiers live and refresh the
            // popup (e.g. holding Cmd shows ⌘). The event passes through (untouched).
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingCombo {
                let flags = CGEventGetFlags(event) as u32;
                *REC_MODS.lock().unwrap() =
                    flags & (0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000);
                notify_main(sel!(handleRecordingStage:));
            }
            event
        }
        10 => {
            // keyDown: only handled while waiting for the combo.
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingCombo {
                let keycode = CGEventGetIntegerValueField(event, 9) as u16;
                let flags = CGEventGetFlags(event) as u32;
                let mods = flags & (0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000);
                // Bare Esc cancels the recording.
                if keycode == 53 && mods == 0 {
                    finish_recording(false);
                    return std::ptr::null_mut();
                }
                let desc = describe_shortcut(keycode, mods);
                *REC_DESC.lock().unwrap() = desc;
                finish_recording(true);
                return std::ptr::null_mut();
            }
            event
        }
        _ => event,
    }
}

/// Recording thread: a dedicated HID-level tap captures the button/keyboard input (does not
/// interfere with the mouse tap or the switcher tap; the RECORDING flag already makes the
/// mouse tap skip binding execution). The RunLoop is stopped on finish/cancel.
pub(super) unsafe fn recording_thread() {
    let rl = crate::event_tap::CFRunLoopGetCurrent();
    *REC_RUNLOOP.0.lock().unwrap() = Some(rl);
    // otherMouseDown(25) | keyDown(10) | flagsChanged(12)
    let mask: crate::event_tap::CGEventMask = (1u64 << 25) | (1u64 << 10) | (1u64 << 12);
    let created = crate::event_tap::create_tap_with_retry(
        crate::event_tap::tap_location::HID_EVENT_TAP,
        crate::event_tap::tap_placement::HEAD_INSERT,
        crate::event_tap::tap_options::DEFAULT_TAP,
        mask,
        Some(recording_tap_callback),
        std::ptr::null_mut(),
        "rec",
        Some(&REC_CANCEL),
    );
    let Some(created) = created else {
        // Tap creation failed (missing permission etc.): reset state, notify cancel.
        *REC_STAGE.lock().unwrap() = RecStage::Idle;
        crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
        *REC_RUNLOOP.0.lock().unwrap() = None;
        notify_main(sel!(handleRecordingCancelled:));
        return;
    };
    *REC_TAP.0.lock().unwrap() = Some(created.tap);
    let watchdog = crate::event_tap::start_tap_watchdog(created.tap, &REC_CANCEL);
    log_debug!("[mouse] recording tap started");
    if !REC_CANCEL.load(std::sync::atomic::Ordering::SeqCst) && crate::input_monitor::taps_allowed()
    {
        crate::event_tap::CFRunLoopRun();
    }
    crate::event_tap::stop_tap_watchdog(watchdog);
    crate::event_tap::CGEventTapEnable(created.tap, false);
    *REC_TAP.0.lock().unwrap() = None;
    *REC_RUNLOOP.0.lock().unwrap() = None;
    crate::event_tap::teardown_event_tap(rl, created);
    log_debug!("[mouse] recording tap stopped");
}

/// The delete button (tag = button number): removes that mapping.
/// The "Add mapping" button: opens the mapping edit panel (trigger/action/combo configured
/// in one place, LinearMouse style).
pub(crate) extern "C" fn handle_add_mapping(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // Panel already open: close it first.
    if EDIT_PANEL.lock().unwrap().is_some() {
        close_mapping_panel();
    }
    open_mapping_panel(None);
    log_debug!("[mouse] mapping panel opened (new mapping)");
}

/// The row "Edit" callback (tag = button number): opens the panel prefilled.
pub(crate) extern "C" fn handle_mapping_edit(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    // Panel already open: close it first (the user expects a fresh panel, not silence).
    if EDIT_PANEL.lock().unwrap().is_some() {
        close_mapping_panel();
    }
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    open_mapping_panel(Some(tag as u32));
    log_debug!("[mouse] mapping panel opened (edit button {})", tag);
}

/// The panel "Record trigger" button: records the side button.
pub(crate) extern "C" fn handle_panel_record_trigger(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        return;
    }
    *REC_BUTTON.lock().unwrap() = 0;
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    *REC_MODE.lock().unwrap() = RecMode::PanelTrigger;
    *REC_STAGE.lock().unwrap() = RecStage::WaitingButton;
    REC_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(true, Ordering::Relaxed);
    // Disable the panel OK while recording.
    unsafe {
        if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
            let _: () = msg_send![o.0, setEnabled: false];
        }
    }
    log_debug!("[mouse] recording trigger (press a mouse button)");
    *RECORD_THREAD.lock().unwrap() = Some(std::thread::spawn(|| unsafe { recording_thread() }));
}

/// The panel "Record combo" button: records the combo (Key Press action).
pub(crate) extern "C" fn handle_panel_record_combo(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        return;
    }
    // The trigger must exist first (for new mappings).
    let Some(btn) = *EDIT_BUTTON.lock().unwrap() else {
        return;
    };
    *REC_BUTTON.lock().unwrap() = btn;
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    *REC_MODE.lock().unwrap() = RecMode::PanelCombo;
    *REC_STAGE.lock().unwrap() = RecStage::WaitingCombo;
    REC_CANCEL.store(false, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(true, Ordering::Relaxed);
    unsafe {
        if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
            let _: () = msg_send![o.0, setEnabled: false];
        }
    }
    log_debug!("[mouse] recording combo (press the key combo)");
    *RECORD_THREAD.lock().unwrap() = Some(std::thread::spawn(|| unsafe { recording_thread() }));
}

/// The panel action popup changed: refresh the combo row and OK availability.
pub(crate) extern "C" fn handle_panel_action_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, indexOfSelectedItem] };
    *EDIT_ACTION_IDX.lock().unwrap() = idx;
    // Leaving Key Press clears the recorded combo.
    if idx != 2 {
        EDIT_COMBO.lock().unwrap().clear();
    }
    unsafe {
        update_mapping_panel();
    }
}

/// The panel "OK": write to MAPPING_EDITS and close.
pub(crate) extern "C" fn handle_mapping_confirm(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    let Some(btn) = *EDIT_BUTTON.lock().unwrap() else {
        return;
    };
    let idx = *EDIT_ACTION_IDX.lock().unwrap();
    let mut edits = MAPPING_EDITS.lock().unwrap();
    match idx {
        0 => {
            // Default: same as delete (the list shows bound rows only).
            edits.remove(&btn.to_string());
        }
        1 => {
            edits.insert(btn.to_string(), "none".to_string());
        }
        2 => {
            // Key Press: needs a recorded combo (OK is disabled otherwise).
            let combo = EDIT_COMBO.lock().unwrap().clone();
            if combo.is_empty() {
                return;
            }
            edits.insert(btn.to_string(), combo);
        }
        7 => {
            // Open the switcher.
            edits.insert(btn.to_string(), "switcher".to_string());
        }
        i => {
            if let Some(name) = crate::mouse::shortcut::SYSTEM_ACTIONS.get((i - 3) as usize) {
                edits.insert(btn.to_string(), name.to_string());
            }
        }
    }
    drop(edits);
    commit_mapping_edits();
    close_mapping_panel();
    render_mapping_rows();
    log_debug!(
        "[mouse] mapping panel confirmed: button {} -> index {}",
        btn,
        idx
    );
}

/// The panel "Cancel": close without changes.
pub(crate) extern "C" fn handle_mapping_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    close_mapping_panel();
    log_debug!("[mouse] mapping panel cancelled");
}

/// The mappings master switch toggled: re-render the rows (greyed out and inert when off).
pub(crate) extern "C" fn handle_mapping_enabled_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    // The mappings master switch is per-device: write it back to the selected device's profile
    // immediately (with a debounced persist), then re-render the row states.
    unsafe { apply_mouse_profile_field(ControlField::MappingEnabled) };
    render_mapping_rows();
    log_debug!("[mouse] mappings master switch toggled");
}

/// Open the mapping edit panel. btn = the button being edited (Some = editing an existing
/// mapping, None = adding a new one). New mappings start by recording the side button;
/// existing ones are prefilled.
pub(super) fn open_mapping_panel(btn: Option<u32>) {
    unsafe {
        *EDIT_BUTTON.lock().unwrap() = btn;
        *EDIT_COMBO.lock().unwrap() = String::new();
        // Prefill: for an existing mapping, derive the action index and combo from the
        // current value.
        let (action_idx, combo) = match btn {
            Some(b) => match MAPPING_EDITS.lock().unwrap().get(&b.to_string()) {
                Some(desc) => {
                    use crate::mouse::shortcut::{Binding, SYSTEM_ACTIONS};
                    match crate::mouse::shortcut::parse_binding(desc) {
                        Ok(Binding::Key(_)) => (2, desc.clone()),
                        Ok(Binding::System(_)) => (
                            SYSTEM_ACTIONS
                                .iter()
                                .position(|a| a.eq_ignore_ascii_case(desc))
                                .map(|i| i as isize + 3)
                                .unwrap_or(0),
                            String::new(),
                        ),
                        Ok(Binding::Switcher) => (7, String::new()),
                        Ok(Binding::None) => (1, String::new()),
                        Err(_) => (0, String::new()),
                    }
                }
                None => (0, String::new()),
            },
            None => (0, String::new()),
        };
        *EDIT_ACTION_IDX.lock().unwrap() = action_idx;
        *EDIT_COMBO.lock().unwrap() = combo;

        let existing = EDIT_PANEL.lock().unwrap().map(|p| p.0);
        let panel = if let Some(p) = existing {
            p
        } else {
            // Create the panel: rounded material + trigger/action/combo rows + cancel/OK.
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(440.0, 240.0));
            let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
            let panel: *mut AnyObject = msg_send![panel, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
            apply_settings_window_appearance(panel);
            let _: () = msg_send![panel, setReleasedWhenClosed: false];
            let _: () = msg_send![panel, setOpaque: false];
            let _: () = msg_send![panel, setLevel: 3isize]; // NSFloatingWindowLevel
                                                            // Transparent background: the corners outside the radius show what's behind
                                                            // (the dim layer / settings window), making the rounding visible.
            let clear_ns: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![panel, setBackgroundColor: clear_ns];
            // Background: a plain view + windowBackgroundColor (same color and mechanism as
            // the settings window's content area -- its CGColor works; only
            // controlBackgroundColor's dynamic color is nil).
            let ve: *mut AnyObject = msg_send![class!(NSView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: frame];
            let _: () = msg_send![ve, setWantsLayer: true];
            let ve_layer: *mut AnyObject = msg_send![ve, layer];
            let _: () = msg_send![ve_layer, setCornerRadius: 10.0f64];
            let _: () = msg_send![ve_layer, setMasksToBounds: true];
            layer_set_background(
                ve_layer,
                crate::ffi::hex_to_cg_color(settings_palette().window_bg),
            );
            let _: () = msg_send![panel, setContentView: ve];
            release_obj(ve);
            let target = MENU_TARGET.lock().unwrap().unwrap().0;
            // The trigger row.
            let t_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let t_label: *mut AnyObject = msg_send![t_label, initWithFrame: NSRect::new(NSPoint::new(16.0, 190.0), NSSize::new(110.0, 24.0))];
            set_field(t_label, 0);
            let _: () = msg_send![t_label, setBezeled: false];
            let _: () = msg_send![t_label, setDrawsBackground: false];
            let _: () = msg_send![t_label, setEditable: false];
            apply_settings_text_role(t_label, SettingsTextRole::Secondary);
            let t_label_ns = make_nsstring(&t("settings.mapping_panel_trigger"));
            let _: () = msg_send![t_label, setStringValue: t_label_ns];
            CFRelease(t_label_ns as *const c_void);
            let _: () = msg_send![ve, addSubview: t_label];
            release_obj(t_label);
            let btn_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let btn_label: *mut AnyObject = msg_send![btn_label, initWithFrame: NSRect::new(NSPoint::new(130.0, 190.0), NSSize::new(140.0, 24.0))];
            set_field(btn_label, 0);
            let _: () = msg_send![btn_label, setBezeled: false];
            let _: () = msg_send![btn_label, setDrawsBackground: false];
            let _: () = msg_send![btn_label, setEditable: false];
            apply_settings_text_role(btn_label, SettingsTextRole::Primary);
            let _: () = msg_send![ve, addSubview: btn_label];
            release_obj(btn_label);
            let rec_btn = SettingsButton::action(
                NSRect::new(NSPoint::new(280.0, 190.0), NSSize::new(140.0, 24.0)),
                &t("settings.mapping_record"),
                target,
                sel!(handlePanelRecordTrigger:),
                SettingsButtonRole::Action,
            );
            let _: () = msg_send![ve, addSubview: rec_btn];
            release_obj(rec_btn);
            // The action row.
            let a_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let a_label: *mut AnyObject = msg_send![a_label, initWithFrame: NSRect::new(NSPoint::new(16.0, 140.0), NSSize::new(110.0, 24.0))];
            set_field(a_label, 0);
            let _: () = msg_send![a_label, setBezeled: false];
            let _: () = msg_send![a_label, setDrawsBackground: false];
            let _: () = msg_send![a_label, setEditable: false];
            apply_settings_text_role(a_label, SettingsTextRole::Secondary);
            let a_label_ns = make_nsstring(&t("settings.mapping_panel_action"));
            let _: () = msg_send![a_label, setStringValue: a_label_ns];
            CFRelease(a_label_ns as *const c_void);
            let _: () = msg_send![ve, addSubview: a_label];
            release_obj(a_label);
            let popup_items: Vec<String> = MAPPING_ACTION_KEYS.iter().map(|k| t(k)).collect();
            let popup_items: Vec<&str> = popup_items.iter().map(|s| s.as_str()).collect();
            let action: *mut AnyObject = make_popup(130.0, 140.0, 290.0, 26.0, &popup_items, 0);
            let _: () = msg_send![action, setTarget: target];
            let _: () = msg_send![action, setAction: sel!(handlePanelActionChanged:)];
            // Popup icons (same as the rows).
            for i in 0..MAPPING_ACTION_KEYS.len() {
                let Some(icon) = SettingsMappingActionIcon::symbol_name(i) else {
                    continue;
                };
                settings_select_set_item_symbol(action, i, icon);
            }
            let _: () = msg_send![ve, addSubview: action];
            release_obj(action);
            // The combo row (shown for Key Press).
            let combo_btn = SettingsButton::action(
                NSRect::new(NSPoint::new(130.0, 96.0), NSSize::new(140.0, 24.0)),
                &t("settings.mapping_record"),
                target,
                sel!(handlePanelRecordCombo:),
                SettingsButtonRole::Action,
            );
            let _: () = msg_send![combo_btn, setHidden: true];
            let _: () = msg_send![ve, addSubview: combo_btn];
            release_obj(combo_btn);
            let combo_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let combo_label: *mut AnyObject = msg_send![combo_label, initWithFrame: NSRect::new(NSPoint::new(280.0, 96.0), NSSize::new(140.0, 24.0))];
            set_field(combo_label, 0);
            let _: () = msg_send![combo_label, setBezeled: false];
            let _: () = msg_send![combo_label, setDrawsBackground: false];
            let _: () = msg_send![combo_label, setEditable: false];
            apply_settings_text_role(combo_label, SettingsTextRole::Primary);
            let _: () = msg_send![combo_label, setHidden: true];
            let _: () = msg_send![ve, addSubview: combo_label];
            release_obj(combo_label);
            // Cancel/OK.
            let cancel = SettingsButton::action(
                NSRect::new(NSPoint::new(240.0, 24.0), NSSize::new(88.0, 28.0)),
                &t("settings.recording_cancel"),
                target,
                sel!(handleMappingCancel:),
                SettingsButtonRole::Footer,
            );
            let _: () = msg_send![ve, addSubview: cancel];
            release_obj(cancel);
            let ok = SettingsButton::action(
                NSRect::new(NSPoint::new(336.0, 24.0), NSSize::new(88.0, 28.0)),
                &t("settings.ok"),
                target,
                sel!(handleMappingConfirm:),
                SettingsButtonRole::Primary,
            );
            let _: () = msg_send![ve, addSubview: ok];
            release_obj(ok);
            *EDIT_PANEL.lock().unwrap() = Some(ObjPtr::new(panel));
            *EDIT_PANEL_BTN_LABEL.lock().unwrap() = Some(ObjPtr::new(btn_label));
            *EDIT_PANEL_ACTION.lock().unwrap() = Some(ObjPtr::new(action));
            *EDIT_PANEL_COMBO_BTN.lock().unwrap() = Some(ObjPtr::new(combo_btn));
            *EDIT_PANEL_COMBO_LABEL.lock().unwrap() = Some(ObjPtr::new(combo_label));
            *EDIT_PANEL_OK.lock().unwrap() = Some(ObjPtr::new(ok));
            panel
        };
        // Update the panel display.
        update_mapping_panel();
        // Position: centered on the settings window (does not drift with the screen).
        let win = super::with_settings_ui(|ui| ui.as_ref().unwrap().window);
        let win_frame: NSRect = msg_send![win, frame];
        let pf: NSRect = msg_send![panel, frame];
        let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(
            win_frame.origin.x + (win_frame.size.width - pf.size.width) / 2.0,
            win_frame.origin.y + (win_frame.size.height - pf.size.height) / 2.0
        )];
        // The dim layer: a translucent gray overlay on the settings content (modal dim;
        // the panel floats above it).
        let content: *mut AnyObject = msg_send![win, contentView];
        let content_bounds: NSRect = msg_send![content, bounds];
        let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
        let dim: *mut AnyObject = msg_send![dim, initWithFrame: content_bounds];
        let _: () = msg_send![dim, setWantsLayer: true];
        let dim_layer: *mut AnyObject = msg_send![dim, layer];
        // Translucent black at 25%: hex is 0xRRGGBBAA -- alpha lives in the low byte.
        layer_set_background(dim_layer, hex_to_cg_color(0x00000040));
        let _: () = msg_send![content, addSubview: dim];
        release_obj(dim);
        *EDIT_DIM.lock().unwrap() = Some(ObjPtr::new(dim));
        let _: () = msg_send![panel, orderFrontRegardless];
    }
}

/// Refresh the panel display (trigger name / action popup / combo row / OK availability).
pub(super) unsafe fn update_mapping_panel() {
    let btn = *EDIT_BUTTON.lock().unwrap();
    // The trigger button name.
    if let Some(l) = *EDIT_PANEL_BTN_LABEL.lock().unwrap() {
        let text = match btn {
            Some(b) => crate::mouse::shortcut::button_name(b),
            None => t("settings.mapping_panel_no_button"),
        };
        let ns = make_nsstring(&text);
        let _: () = msg_send![l.0, setStringValue: ns];
        CFRelease(ns as *const c_void);
    }
    let idx = *EDIT_ACTION_IDX.lock().unwrap();
    // The action popup.
    if let Some(a) = *EDIT_PANEL_ACTION.lock().unwrap() {
        let _: () = msg_send![a.0, selectItemAtIndex: idx];
    }
    // Combo row visibility (Key Press = index 2).
    let is_key = idx == 2;
    if let Some(b) = *EDIT_PANEL_COMBO_BTN.lock().unwrap() {
        let _: () = msg_send![b.0, setHidden: !is_key];
    }
    if let Some(l) = *EDIT_PANEL_COMBO_LABEL.lock().unwrap() {
        let _: () = msg_send![l.0, setHidden: !is_key];
        if is_key {
            let combo = EDIT_COMBO.lock().unwrap().clone();
            let text = if combo.is_empty() {
                t("settings.mapping_panel_no_combo")
            } else {
                display_shortcut(&combo)
            };
            let ns = make_nsstring(&text);
            let _: () = msg_send![l.0, setStringValue: ns];
            CFRelease(ns as *const c_void);
        }
    }
    // OK availability: Key Press needs a recorded combo; a new mapping needs the trigger.
    let ok_enabled =
        (idx != 2 || !EDIT_COMBO.lock().unwrap().is_empty()) && (btn.is_some() || idx != 2);
    if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
        let _: () = msg_send![o.0, setEnabled: ok_enabled];
    }
}

/// Close the mapping edit panel (idempotent).
pub(super) fn close_mapping_panel() {
    // take(): an if-let scrutinee MutexGuard lives for the WHOLE if block, so locking the
    // same Mutex inside it self-deadlocks (the beach ball, verified). take() moves the
    // value out and the guard drops immediately.
    if let Some(p) = EDIT_PANEL.lock().unwrap().take() {
        unsafe {
            let _: () = msg_send![p.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    // Remove the dim layer.
    if let Some(d) = EDIT_DIM.lock().unwrap().take() {
        unsafe {
            let _: () = msg_send![d.0, removeFromSuperview];
        }
    }
    *EDIT_BUTTON.lock().unwrap() = None;
    *EDIT_ACTION_IDX.lock().unwrap() = 0;
    EDIT_COMBO.lock().unwrap().clear();
}

/// The delete button (tag = button number): removes that mapping.
pub(crate) extern "C" fn handle_delete_mapping(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    MAPPING_EDITS.lock().unwrap().remove(&tag.to_string());
    commit_mapping_edits();
    render_mapping_rows();
    log_debug!("[mouse] removed mapping for button {}", tag);
}

pub(crate) extern "C" fn handle_recording_finished(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    let btn = *REC_BUTTON.lock().unwrap();
    match *REC_MODE.lock().unwrap() {
        // The panel recorded the trigger: update the panel.
        RecMode::PanelTrigger => {
            *EDIT_BUTTON.lock().unwrap() = Some(btn);
            unsafe {
                update_mapping_panel();
            }
            log_debug!("[mouse] panel trigger recorded: button {}", btn);
        }
        // The panel recorded the combo: update the panel (Key Press action).
        RecMode::PanelCombo => {
            let desc = REC_DESC.lock().unwrap().clone();
            *EDIT_COMBO.lock().unwrap() = desc.clone();
            unsafe {
                update_mapping_panel();
            }
            log_debug!("[mouse] panel combo recorded: {}", desc);
        }
    }
}

/// Main-thread callback: recording cancelled/failed.
pub(crate) extern "C" fn handle_recording_cancelled(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    log_debug!("[mouse] button-mapping recording cancelled");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// notify_main runs on the recording thread and must be safe off the main thread. The old
    /// implementation read the main-thread-only MENU_TARGET, which trips the main-thread assertion
    /// in debug builds -- and since the caller is an extern "C" event-tap callback, that panic
    /// cannot unwind and aborts the process (recording a side button always crashed). Calling it
    /// once from a background thread fails this test (join returns Err) if the assertion comes back.
    #[test]
    fn notify_main_is_safe_off_the_main_thread() {
        // No settings target exists in a unit test (the assembly never runs), so the function takes
        // the early "no target" path -- but the assertion happened *before* reading it, which is
        // exactly the step this guards.
        std::thread::spawn(|| notify_main(sel!(handleRecordingFinished:)))
            .join()
            .expect("notify_main must not panic when called off the main thread");
    }
}
