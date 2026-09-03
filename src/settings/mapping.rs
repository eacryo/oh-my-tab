//! 设置窗口 · 按键映射:映射行渲染、侧键录制(独立 CGEventTap 线程)与编辑面板。
//! Button mappings: mapping-row rendering, side-button recording (dedicated CGEventTap thread), and the edit panel.

use super::*;

// ========== 按键映射录制 / button-mapping recording ==========

/// 渲染当前设备的按键映射行到滚动容器(录制/删除/设备切换后调用)。
/// 清掉旧行后按按钮号排序重建;mapping_doc 是 flipped 视图,行从顶向下排。
///
/// Render the selected device's button-mapping rows into the scroll container (called after
/// recording / deletion / device switch). Old rows are removed first, then rebuilt sorted by
/// button number; mapping_doc is flipped, so rows stack top-down.
pub(super) fn render_mapping_rows() {
    unsafe {
        let mut guard = SETTINGS_UI.lock().unwrap();
        let Some(u) = guard.as_mut() else { return };
        render_mapping_rows_locked(u);
    }
}

/// 持锁版本:调用方已持有 SETTINGS_UI 锁时使用(load_settings_from / handle_device_changed),
/// 避免对同一把非重入 Mutex 二次加锁自死锁。
///
/// Locked variant: used when the caller already holds the SETTINGS_UI lock
/// (load_settings_from / handle_device_changed), avoiding a self-deadlock on the same
/// non-reentrant Mutex.
pub(super) unsafe fn render_mapping_rows_locked(u: &mut SettingsUi) {
    unsafe {
        // removeFromSuperview 已释放父视图持有的引用;创建时的 alloc +1 已在 addSubview 后
        // 用 release_obj 平衡过。这里绝不能再次 release —— 双重释放会 EXC_BAD_ACCESS。
        // removeFromSuperview already drops the superview's reference; the alloc +1 was
        // balanced by release_obj right after addSubview. Re-releasing here would double-free
        // (EXC_BAD_ACCESS).
        let stale = u.mapping_rows.len();
        for row in u.mapping_rows.drain(..) {
            let _: () = msg_send![row.label, removeFromSuperview];
            let _: () = msg_send![row.desc_label, removeFromSuperview];
            let _: () = msg_send![row.edit, removeFromSuperview];
            let _: () = msg_send![row.delete, removeFromSuperview];
            for cap in row.caps {
                let _: () = msg_send![cap, removeFromSuperview];
            }
            let _: () = msg_send![row.separator, removeFromSuperview];
        }
        let doc = u.mapping_doc;
        // 列表只显示已绑定的行 + 刚添加未配置的临时行(方案 A 的行内配置,动态行)。
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
        // 卡片高度随行数增长(顶部固定):少行时保持 3 行,多行时向下长高,整页滚动。
        // The card height grows with the row count (top-anchored): it keeps three rows when
        // short and grows downward when long, the page scroll view handles the overflow.
        // 空状态提示:无行时显示。
        // Empty-state hint: shown when there are no rows.
        let _: () = msg_send![u.mapping_empty, setHidden: !items.is_empty()];
        // 只改高度,宽度保持初始值:曾用 setFrameSize(0.0, doc_h) 把宽清零,
        // 宽度为 0 的文档视图 hit-test 失败 —— 行内删除按钮永远点不到。
        // Resize height only, keeping the initial width: setFrameSize(0.0, doc_h) used to
        // zero the width, and a zero-width document view fails hit-testing -- the delete
        // buttons became unclickable.
        // flipped:y=0 在顶部,行从顶部依次向下排。
        // Flipped: y=0 is the top; rows stack down from the top.
        let mappings_on = {
            let st: isize = msg_send![u.mapping_enabled, state];
            st == 1
        };
        // 添加按钮一并置灰(开关关闭时不可添加新映射)。
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
        // Rows start below the header band (MAPPING_PANEL_TOP + MAPPING_HEADER_H).
        let mut y = MAPPING_PANEL_TOP + MAPPING_HEADER_H;
        let target = MENU_TARGET.lock().unwrap().unwrap().0;
        for (btn, desc) in items {
            // 动作类型 index:默认/无/系统动作/快捷键(快捷键 = Key Press)。
            // Action-type index: default / none / system action / shortcut (Key Press).
            let (action_idx, is_key) = match crate::mouse::shortcut::parse_binding(&desc) {
                Ok(crate::mouse::shortcut::Binding::Key(_)) => (2, true),
                Ok(crate::mouse::shortcut::Binding::System(_)) => {
                    // 系统动作名映射到下拉 index(3..=6)。
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
            // 按钮名。
            // The button name.
            // NSTextField 的 13pt 文字在 28pt 框内偏顶部(非垂直居中):label 下移 7pt
            // 让文字中线与右侧按钮文字对齐(实测校准)。
            // The 13pt text sits toward the TOP of the 28pt field (not vertically centered):
            // shifting the label down 7pt aligns its midline with the buttons' (calibrated).
            let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(NSPoint::new(row_x0, y + (row_h - 22.0) / 2.0), NSSize::new(70.0, 22.0))];
            set_field(label, 0);
            let _: () = msg_send![label, setBezeled: false];
            let _: () = msg_send![label, setDrawsBackground: false];
            let _: () = msg_send![label, setEditable: false];
            let name_ns = make_nsstring(&button_name(btn));
            let _: () = msg_send![label, setStringValue: name_ns];
            CFRelease(name_ns as *const c_void);
            let _: () = msg_send![label, setEnabled: mappings_on];
            let _: () = msg_send![doc, addSubview: label];
            release_obj(label);
            // 动作描述:系统动作/None 显示文本;Key Press 显示键帽胶囊。
            // The action description: text for system actions/None; keycaps for Key Press.
            let desc_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let desc_label: *mut AnyObject = msg_send![desc_label, initWithFrame: NSRect::new(NSPoint::new(desc_x, y + (row_h - 22.0) / 2.0), NSSize::new((ed_x - desc_x - 8.0).max(1.0), 22.0))];
            set_field(desc_label, 0);
            let _: () = msg_send![desc_label, setBezeled: false];
            let _: () = msg_send![desc_label, setDrawsBackground: false];
            let _: () = msg_send![desc_label, setEditable: false];
            let _: () = msg_send![desc_label, setEnabled: mappings_on];
            if !is_key && action_idx > 0 {
                // 系统动作/None 的动作名文本(用 i18n 标签)。
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
            // 编辑按钮(打开编辑面板)。
            // The edit button (opens the edit panel).
            let edit: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let edit: *mut AnyObject = msg_send![edit, initWithFrame: NSRect::new(NSPoint::new(ed_x, y + (row_h - btn_h) / 2.0), NSSize::new(btn_w, btn_h))];
            style_html_button(edit, 0x7676801Fu32, 0x44444AFFu32);
            let _: () = msg_send![edit, setTag: btn as isize];
            let _: () = msg_send![edit, setEnabled: mappings_on];
            let _: () = msg_send![edit, setTarget: target];
            let _: () = msg_send![edit, setAction: sel!(handleMappingEdit:)];
            let edit_title = make_nsstring(&t("settings.mapping_edit"));
            let _: () = msg_send![edit, setTitle: edit_title];
            CFRelease(edit_title as *const c_void);
            let _: () = msg_send![doc, addSubview: edit];
            release_obj(edit);
            // 删除按钮(文字样式,与编辑按钮同款)。
            // The delete button (text style, same look as Edit).
            let delete: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let delete: *mut AnyObject = msg_send![delete, initWithFrame: NSRect::new(NSPoint::new(del_x, y + (row_h - btn_h) / 2.0), NSSize::new(btn_w, btn_h))];
            style_html_button(delete, 0x7676801Fu32, 0x44444AFFu32);
            let _: () = msg_send![delete, setTag: btn as isize];
            let _: () = msg_send![delete, setEnabled: mappings_on];
            let _: () = msg_send![delete, setTarget: target];
            let _: () = msg_send![delete, setAction: sel!(handleDeleteMapping:)];
            let del_title = make_nsstring(&t("settings.mapping_delete"));
            let _: () = msg_send![delete, setTitle: del_title];
            CFRelease(del_title as *const c_void);
            let _: () = msg_send![doc, addSubview: delete];
            release_obj(delete);
            // 键帽胶囊:修饰符号 + 主键各一个圆角小方块(像真实键盘键帽)。
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
                    let _: () = msg_send![cap, setAlignment: 1isize]; // center
                    let _: () = msg_send![cap, setEnabled: mappings_on];
                    let ch_ns = make_nsstring(&ch.to_string());
                    let _: () = msg_send![cap, setStringValue: ch_ns];
                    CFRelease(ch_ns as *const c_void);
                    // 圆角浅灰底。
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
            // 行底分隔线(最后一行被卡片底圆角裁掉,无妨)。
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
    }
}

// ========== 录制弹出浮窗 / recording popup panel ==========

/// 经 performSelectorOnMainThread 唤醒主线程上的设置回调(无参版本)。
/// Wake the settings callback on the main thread (argument-less variant).
pub(super) fn notify_main(sel: Sel) {
    if let Some(t) = *MENU_TARGET.lock().unwrap() {
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

/// 录制完成/取消的公共收尾:复位状态与 RECORDING 标志、停止录制线程的 RunLoop、
/// 通知主线程回调。在录制 tap 线程上调用。
///
/// Common teardown for recording finish/cancel: reset the stage and the RECORDING flag,
/// stop the recording thread's RunLoop, and wake the main-thread callback. Called on the
/// recording tap thread.
/// 防御性取消录制:设置窗口 OK/Cancel/关闭时若仍在录制(面板录制中),复位状态,
/// 避免残留录制态影响下次使用。
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

/// 立即禁用录制 tap(若存在)。禁用是同步生效的,CGEventTapEnable(false) 后该 tap
/// 不再收到任何事件。
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
    // 完成/取消后清零中间态,杜绝下次录制的残留(见 handle_add_mapping 的注释)。
    // Clear the intermediates on finish/cancel, so nothing leaks into the next session.
    *REC_MODS.lock().unwrap() = 0;
    REC_DESC.lock().unwrap().clear();
    REC_CANCEL.store(true, std::sync::atomic::Ordering::Relaxed);
    crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
    // 先禁用 tap 再停 runloop:禁用立即生效,杜绝退出窗口期吞键。
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

/// 录制 tap 回调(录制线程):捕获组合键 keyDown(esc 无修饰 = 取消)后完成。
/// 录制输入(组合键)吞掉,flagsChanged 透传并实时刷新浮窗修饰显示。
///
/// Recording tap callback (recording thread): a combo keyDown finishes the recording
/// (bare Esc cancels). The combo input is swallowed; flagsChanged passes through while
/// refreshing the popup's modifier display live.
pub(super) unsafe extern "C" fn recording_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: CGEventRef,
    _user_info: *mut c_void,
) -> CGEventRef {
    match event_type {
        25 => {
            // otherMouseDown; 按钮号在 field 3(与 mouse/event_tap.rs 同)。
            // 只在 WaitingButton 阶段捕获/吞;取消后残留的 tap(若有)一律透传。
            // Button number lives in field 3 (same as mouse/event_tap.rs). Only capture/
            // swallow while WaitingButton; a lingering tap after cancel passes everything.
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingButton {
                let btn = CGEventGetIntegerValueField(event, 3) as u32;
                if btn >= 2 {
                    *REC_BUTTON.lock().unwrap() = btn;
                    // 面板录触发:捕获侧键即完成,回调更新面板。
                    // Panel trigger recording: the side button completes it; the callback
                    // updates the panel.
                    finish_recording(true);
                    return std::ptr::null_mut();
                }
            }
            event
        }
        12 => {
            // flagsChanged:WaitingCombo 阶段实时累积修饰键,刷新浮窗显示(如按住 Cmd 显示 ⌘)。
            // 不吞事件(透传,用户仍可正常操作)。
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
            // keyDown:仅等待组合键阶段处理。
            // keyDown: only handled while waiting for the combo.
            if *REC_STAGE.lock().unwrap() == RecStage::WaitingCombo {
                let keycode = CGEventGetIntegerValueField(event, 9) as u16;
                let flags = CGEventGetFlags(event) as u32;
                let mods = flags & (0x0010_0000 | 0x0008_0000 | 0x0004_0000 | 0x0002_0000);
                // 无修饰的 Esc = 取消录制。
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

/// 录制线程:独立 HID 层 tap 捕获按键/键盘(不干扰鼠标 tap 与窗口切换 tap;
/// RECORDING 标志已让鼠标 tap 跳过绑定执行)。RunLoop 在完成/取消时被停止。
///
/// Recording thread: a dedicated HID-level tap captures the button/keyboard input (does not
/// interfere with the mouse tap or the switcher tap; the RECORDING flag already makes the
/// mouse tap skip binding execution). The RunLoop is stopped on finish/cancel.
pub(super) unsafe fn recording_thread() {
    let rl = crate::event_tap::CFRunLoopGetCurrent();
    *REC_RUNLOOP.0.lock().unwrap() = Some(rl);
    // otherMouseDown(25) | keyDown(10) | flagsChanged(12)
    let mask: crate::event_tap::CGEventMask = (1u64 << 25) | (1u64 << 10) | (1u64 << 12);
    let tap = crate::event_tap::create_tap_with_retry(
        crate::event_tap::tap_location::HID_EVENT_TAP,
        crate::event_tap::tap_placement::HEAD_INSERT,
        crate::event_tap::tap_options::DEFAULT_TAP,
        mask,
        Some(recording_tap_callback),
        std::ptr::null_mut(),
        "rec",
        Some(&REC_CANCEL),
    );
    let Some(tap) = tap else {
        // tap 创建失败(缺权限等):复位状态,让主线程提示取消。
        // Tap creation failed (missing permission etc.): reset state, notify cancel.
        *REC_STAGE.lock().unwrap() = RecStage::Idle;
        crate::mouse::event_tap::RECORDING.store(false, Ordering::Relaxed);
        *REC_RUNLOOP.0.lock().unwrap() = None;
        notify_main(sel!(handleRecordingCancelled:));
        return;
    };
    let source = crate::event_tap::CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
    crate::event_tap::CFRunLoopAddSource(rl, source, crate::event_tap::kCFRunLoopDefaultMode);
    crate::event_tap::CGEventTapEnable(tap, true);
    *REC_TAP.0.lock().unwrap() = Some(tap);
    log_debug!("[mouse] recording tap started");
    crate::event_tap::CFRunLoopRun();
    *REC_TAP.0.lock().unwrap() = None;
    *REC_RUNLOOP.0.lock().unwrap() = None;
    log_debug!("[mouse] recording tap stopped");
}

/// 删除按钮(tag = 按钮号):移除该映射。
/// The delete button (tag = button number): removes that mapping.
/// 「添加映射」按钮:打开映射编辑面板(触发/动作/组合键在面板里一次配完)。
/// The "Add mapping" button: opens the mapping edit panel (trigger/action/combo configured
/// in one place, LinearMouse style).
pub(crate) extern "C" fn handle_add_mapping(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    // 面板已开:先关再开。
    // Panel already open: close it first.
    if EDIT_PANEL.lock().unwrap().is_some() {
        close_mapping_panel();
    }
    open_mapping_panel(None);
    log_debug!("[mouse] mapping panel opened (new mapping)");
}

/// 列表行「编辑」回调(tag = 按钮号):打开面板预填该按钮的映射。
/// The row "Edit" callback (tag = button number): opens the panel prefilled.
pub(crate) extern "C" fn handle_mapping_edit(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    // 面板已开:先关再开(用户点编辑期望打开新面板,而不是无反应)。
    // Panel already open: close it first (the user expects a fresh panel, not silence).
    if EDIT_PANEL.lock().unwrap().is_some() {
        close_mapping_panel();
    }
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    open_mapping_panel(Some(tag as u32));
    log_debug!("[mouse] mapping panel opened (edit button {})", tag);
}

/// 面板「录制触发」按钮:录侧键。
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
    // 录制中禁用面板确认,防中途误确认。
    // Disable the panel OK while recording.
    unsafe {
        if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
            let _: () = msg_send![o.0, setEnabled: false];
        }
    }
    log_debug!("[mouse] recording trigger (press a mouse button)");
    *RECORD_THREAD.lock().unwrap() = Some(std::thread::spawn(|| unsafe { recording_thread() }));
}

/// 面板「录制组合键」按钮:录组合键(Key Press 动作)。
/// The panel "Record combo" button: records the combo (Key Press action).
pub(crate) extern "C" fn handle_panel_record_combo(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    if *REC_STAGE.lock().unwrap() != RecStage::Idle {
        return;
    }
    // 需要先有触发按钮(新增时)。
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

/// 面板动作下拉变化:更新组合键行显隐与确认可用性。
/// The panel action popup changed: refresh the combo row and OK availability.
pub(crate) extern "C" fn handle_panel_action_changed(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, indexOfSelectedItem] };
    *EDIT_ACTION_IDX.lock().unwrap() = idx;
    // 切到非 Key Press 时清掉已录组合键。
    // Leaving Key Press clears the recorded combo.
    if idx != 2 {
        EDIT_COMBO.lock().unwrap().clear();
    }
    unsafe {
        update_mapping_panel();
    }
}

/// 面板「确认」:写入 MAPPING_EDITS 并关闭。
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
            // Default:等同删除(列表只显示已绑定)。
            // Default: same as delete (the list shows bound rows only).
            edits.remove(&btn.to_string());
        }
        1 => {
            edits.insert(btn.to_string(), "none".to_string());
        }
        2 => {
            // Key Press:需要已录组合键(确认按钮已按可用性禁用)。
            // Key Press: needs a recorded combo (OK is disabled otherwise).
            let combo = EDIT_COMBO.lock().unwrap().clone();
            if combo.is_empty() {
                return;
            }
            edits.insert(btn.to_string(), combo);
        }
        7 => {
            // 打开切换器。
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
    close_mapping_panel();
    render_mapping_rows();
    log_debug!(
        "[mouse] mapping panel confirmed: button {} -> index {}",
        btn,
        idx
    );
}

/// 面板「取消」:直接关闭,不改动。
/// The panel "Cancel": close without changes.
pub(crate) extern "C" fn handle_mapping_cancel(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    close_mapping_panel();
    log_debug!("[mouse] mapping panel cancelled");
}

/// 映射总开关变化回调:重渲染映射行(关闭时行控件置灰不可点)。
/// The mappings master switch toggled: re-render the rows (greyed out and inert when off).
pub(crate) extern "C" fn handle_mapping_enabled_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    render_mapping_rows();
    log_debug!("[mouse] mappings master switch toggled");
}

// ========== 映射编辑面板实现 / mapping edit panel ==========

/// 打开映射编辑面板。btn = 正在编辑的按钮号(Some = 编辑已有映射,None = 新增)。
/// 新增时先从录制侧键开始;编辑时预填当前值。
///
/// Open the mapping edit panel. btn = the button being edited (Some = editing an existing
/// mapping, None = adding a new one). New mappings start by recording the side button;
/// existing ones are prefilled.
pub(super) fn open_mapping_panel(btn: Option<u32>) {
    unsafe {
        *EDIT_BUTTON.lock().unwrap() = btn;
        *EDIT_COMBO.lock().unwrap() = String::new();
        // 预填:编辑已有映射时,按当前值推导动作 index 与组合键。
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
            // 创建面板:圆角毛玻璃 + 触发/动作/组合键行 + 取消确认。
            // Create the panel: rounded material + trigger/action/combo rows + cancel/OK.
            let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(440.0, 240.0));
            let panel: *mut AnyObject = msg_send![class!(NSPanel), alloc];
            let panel: *mut AnyObject = msg_send![panel, initWithContentRect: frame, styleMask: 0u64, backing: 2u64, defer: false];
            apply_settings_window_appearance(panel);
            let _: () = msg_send![panel, setReleasedWhenClosed: false];
            let _: () = msg_send![panel, setOpaque: false];
            let _: () = msg_send![panel, setLevel: 3isize]; // NSFloatingWindowLevel
                                                            // 背景透明:圆角外的四角露出后面的遮罩/设置窗口,圆角才可见。
                                                            // Transparent background: the corners outside the radius show what's behind
                                                            // (the dim layer / settings window), making the rounding visible.
            let clear_ns: *mut AnyObject = msg_send![class!(NSColor), clearColor];
            let _: () = msg_send![panel, setBackgroundColor: clear_ns];
            // 背景:普通视图 + windowBackgroundColor(与设置窗口右侧内容区同款颜色,
            // 同款机制 —— 该颜色的 CGColor 可用;controlBackgroundColor 的动态色才为 nil)。
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
            // 触发行。
            // The trigger row.
            let t_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let t_label: *mut AnyObject = msg_send![t_label, initWithFrame: NSRect::new(NSPoint::new(16.0, 190.0), NSSize::new(110.0, 24.0))];
            set_field(t_label, 0);
            let _: () = msg_send![t_label, setBezeled: false];
            let _: () = msg_send![t_label, setDrawsBackground: false];
            let _: () = msg_send![t_label, setEditable: false];
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
            let _: () = msg_send![ve, addSubview: btn_label];
            release_obj(btn_label);
            let rec_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let rec_btn: *mut AnyObject = msg_send![rec_btn, initWithFrame: NSRect::new(NSPoint::new(280.0, 190.0), NSSize::new(140.0, 24.0))];
            style_html_button(rec_btn, 0xFFFFFFADu32, 0x2E2E2EFFu32);
            let rec_title = make_nsstring(&t("settings.mapping_record"));
            let _: () = msg_send![rec_btn, setTitle: rec_title];
            CFRelease(rec_title as *const c_void);
            let _: () = msg_send![rec_btn, setTarget: target];
            let _: () = msg_send![rec_btn, setAction: sel!(handlePanelRecordTrigger:)];
            let _: () = msg_send![ve, addSubview: rec_btn];
            release_obj(rec_btn);
            // 动作行。
            // The action row.
            let a_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let a_label: *mut AnyObject = msg_send![a_label, initWithFrame: NSRect::new(NSPoint::new(16.0, 140.0), NSSize::new(110.0, 24.0))];
            set_field(a_label, 0);
            let _: () = msg_send![a_label, setBezeled: false];
            let _: () = msg_send![a_label, setDrawsBackground: false];
            let _: () = msg_send![a_label, setEditable: false];
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
            // 下拉图标(与行内同款)。
            // Popup icons (same as the rows).
            let menu: *mut AnyObject = msg_send![action, menu];
            let item_cnt: usize = msg_send![menu, numberOfItems];
            let icons = [
                "dot.circle",
                "slash.circle",
                "keyboard",
                "square.grid.2x2",
                "square.grid.3x3",
                "macwindow",
                "rectangle.on.rectangle",
                "arrow.left.arrow.right",
            ];
            for (i, icon) in icons.iter().enumerate().take(item_cnt) {
                let item: *mut AnyObject = msg_send![menu, itemAtIndex: i as isize];
                let sym = make_nsstring(icon);
                let img: *mut AnyObject = msg_send![
                    class!(NSImage),
                    imageWithSystemSymbolName: sym,
                    accessibilityDescription: std::ptr::null::<AnyObject>()
                ];
                CFRelease(sym as *const c_void);
                if !img.is_null() {
                    let _: () = msg_send![item, setImage: img];
                }
            }
            let _: () = msg_send![ve, addSubview: action];
            release_obj(action);
            // 组合键行(Key Press 时显示)。
            // The combo row (shown for Key Press).
            let combo_btn: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let combo_btn: *mut AnyObject = msg_send![combo_btn, initWithFrame: NSRect::new(NSPoint::new(130.0, 96.0), NSSize::new(140.0, 24.0))];
            style_html_button(combo_btn, 0xFFFFFFADu32, 0x2E2E2EFFu32);
            let combo_title = make_nsstring(&t("settings.mapping_record"));
            let _: () = msg_send![combo_btn, setTitle: combo_title];
            CFRelease(combo_title as *const c_void);
            let _: () = msg_send![combo_btn, setTarget: target];
            let _: () = msg_send![combo_btn, setAction: sel!(handlePanelRecordCombo:)];
            let _: () = msg_send![combo_btn, setHidden: true];
            let _: () = msg_send![ve, addSubview: combo_btn];
            release_obj(combo_btn);
            let combo_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let combo_label: *mut AnyObject = msg_send![combo_label, initWithFrame: NSRect::new(NSPoint::new(280.0, 96.0), NSSize::new(140.0, 24.0))];
            set_field(combo_label, 0);
            let _: () = msg_send![combo_label, setBezeled: false];
            let _: () = msg_send![combo_label, setDrawsBackground: false];
            let _: () = msg_send![combo_label, setEditable: false];
            let _: () = msg_send![combo_label, setHidden: true];
            let _: () = msg_send![ve, addSubview: combo_label];
            release_obj(combo_label);
            // 取消/确认。
            // Cancel/OK.
            let cancel: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let cancel: *mut AnyObject = msg_send![cancel, initWithFrame: NSRect::new(NSPoint::new(240.0, 24.0), NSSize::new(88.0, 28.0))];
            style_html_button(cancel, 0xFFFFFFC7u32, 0x2E2E2EFFu32);
            let cancel_ns = make_nsstring(&t("settings.recording_cancel"));
            let _: () = msg_send![cancel, setTitle: cancel_ns];
            CFRelease(cancel_ns as *const c_void);
            let _: () = msg_send![cancel, setTarget: target];
            let _: () = msg_send![cancel, setAction: sel!(handleMappingCancel:)];
            let _: () = msg_send![ve, addSubview: cancel];
            release_obj(cancel);
            let ok: *mut AnyObject = msg_send![class!(NSButton), alloc];
            let ok: *mut AnyObject = msg_send![ok, initWithFrame: NSRect::new(NSPoint::new(336.0, 24.0), NSSize::new(88.0, 28.0))];
            style_html_button(ok, 0x0A84FFFFu32, 0xFFFFFFFFu32);
            let ok_ns = make_nsstring(&t("settings.ok"));
            let _: () = msg_send![ok, setTitle: ok_ns];
            CFRelease(ok_ns as *const c_void);
            let _: () = msg_send![ok, setTarget: target];
            let _: () = msg_send![ok, setAction: sel!(handleMappingConfirm:)];
            let _: () = msg_send![ve, addSubview: ok];
            release_obj(ok);
            *EDIT_PANEL.lock().unwrap() = Some(ObjPtr(panel));
            *EDIT_PANEL_BTN_LABEL.lock().unwrap() = Some(ObjPtr(btn_label));
            *EDIT_PANEL_ACTION.lock().unwrap() = Some(ObjPtr(action));
            *EDIT_PANEL_COMBO_BTN.lock().unwrap() = Some(ObjPtr(combo_btn));
            *EDIT_PANEL_COMBO_LABEL.lock().unwrap() = Some(ObjPtr(combo_label));
            *EDIT_PANEL_OK.lock().unwrap() = Some(ObjPtr(ok));
            panel
        };
        // 更新面板显示。
        // Update the panel display.
        update_mapping_panel();
        // 定位:相对外层设置窗口居中(不随屏幕位置漂移)。
        // Position: centered on the settings window (does not drift with the screen).
        let win = SETTINGS_UI.lock().unwrap().as_ref().unwrap().window;
        let win_frame: NSRect = msg_send![win, frame];
        let pf: NSRect = msg_send![panel, frame];
        let _: () = msg_send![panel, setFrameOrigin: NSPoint::new(
            win_frame.origin.x + (win_frame.size.width - pf.size.width) / 2.0,
            win_frame.origin.y + (win_frame.size.height - pf.size.height) / 2.0
        )];
        // 遮罩:设置窗口内容区上的半透明灰层(modal 调暗;面板在遮罩之上)。
        // The dim layer: a translucent gray overlay on the settings content (modal dim;
        // the panel floats above it).
        let content: *mut AnyObject = msg_send![win, contentView];
        let content_bounds: NSRect = msg_send![content, bounds];
        let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
        let dim: *mut AnyObject = msg_send![dim, initWithFrame: content_bounds];
        let _: () = msg_send![dim, setWantsLayer: true];
        let dim_layer: *mut AnyObject = msg_send![dim, layer];
        // 半透明黑 25%:hex 是 0xRRGGBBAA —— alpha 在最低字节。
        // Translucent black at 25%: hex is 0xRRGGBBAA -- alpha lives in the low byte.
        layer_set_background(dim_layer, hex_to_cg_color(0x00000040));
        let _: () = msg_send![content, addSubview: dim];
        release_obj(dim);
        *EDIT_DIM.lock().unwrap() = Some(ObjPtr(dim));
        let _: () = msg_send![panel, orderFrontRegardless];
    }
}

/// 刷新面板显示(触发按钮名/动作下拉/组合键行/确认可用性)。
/// Refresh the panel display (trigger name / action popup / combo row / OK availability).
pub(super) unsafe fn update_mapping_panel() {
    let btn = *EDIT_BUTTON.lock().unwrap();
    // 触发按钮名。
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
    // 动作下拉。
    // The action popup.
    if let Some(a) = *EDIT_PANEL_ACTION.lock().unwrap() {
        let _: () = msg_send![a.0, selectItemAtIndex: idx];
    }
    // 组合键行显隐(Key Press = index 2)。
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
    // 确认可用性:Key Press 需要已录组合键;新增需要已录侧键。
    // OK availability: Key Press needs a recorded combo; a new mapping needs the trigger.
    let ok_enabled =
        (idx != 2 || !EDIT_COMBO.lock().unwrap().is_empty()) && (btn.is_some() || idx != 2);
    if let Some(o) = *EDIT_PANEL_OK.lock().unwrap() {
        let _: () = msg_send![o.0, setEnabled: ok_enabled];
    }
}

/// 关闭映射编辑面板(幂等)。
/// Close the mapping edit panel (idempotent).
pub(super) fn close_mapping_panel() {
    // take():if-let scrutinee 的 MutexGuard 会贯穿整个 if 块,块内再 lock 同一把
    // Mutex 就是自死锁(风火轮,实测)。take 拿走值后 guard 立即释放。
    // take(): an if-let scrutinee MutexGuard lives for the WHOLE if block, so locking the
    // same Mutex inside it self-deadlocks (the beach ball, verified). take() moves the
    // value out and the guard drops immediately.
    if let Some(p) = EDIT_PANEL.lock().unwrap().take() {
        unsafe {
            let _: () = msg_send![p.0, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    // 移除遮罩。
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

/// 删除按钮(tag = 按钮号):移除该映射。
/// The delete button (tag = button number): removes that mapping.
pub(crate) extern "C" fn handle_delete_mapping(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    MAPPING_EDITS.lock().unwrap().remove(&tag.to_string());
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
        // 面板录触发侧键:更新面板显示。
        // The panel recorded the trigger: update the panel.
        RecMode::PanelTrigger => {
            *EDIT_BUTTON.lock().unwrap() = Some(btn);
            unsafe {
                update_mapping_panel();
            }
            log_debug!("[mouse] panel trigger recorded: button {}", btn);
        }
        // 面板录组合键:更新面板显示(Key Press 动作)。
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

/// 主线程回调:录制取消/失败。
/// Main-thread callback: recording cancelled/failed.
pub(crate) extern "C" fn handle_recording_cancelled(
    _self: *mut c_void,
    _cmd: Sel,
    _arg: *mut c_void,
) {
    log_debug!("[mouse] button-mapping recording cancelled");
}
