//! Clipboard subsystem · notifications: pasteboard notification observers.

use super::*;

/// A singleton notification observer carrying two callbacks:
/// - NSPasteboardDidChangeNotification: record on every pasteboard change. Polling samples
///   the current value once per 0.5s interval, so rapid consecutive copies between samples
///   are skipped (history ends up with only the newest entry); the notification fires on
///   every change, so no event is lost.
/// - NSWindowDidResignKeyNotification: the picker loses key (a click outside) -> hide.
pub(super) unsafe fn observer() -> *mut AnyObject {
    static OBSERVER: OnceLock<CallbackTarget> = OnceLock::new();
    OBSERVER
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardObserver").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(clipboardPasteboardChanged:),
                pasteboard_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(pollClipboardOnMain:),
                clip_poll_on_main as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(applyClipboardLoadOnMain:),
                apply_clipboard_load_on_main as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clipboardWindowResigned:),
                window_did_resign_key as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearSystemPasteboardIfOwned:),
                clear_system_pasteboard_if_owned as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(scrollIndicatorBoundsChanged:),
                scroll_indicator_bounds_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(detailScrollIndicatorBoundsChanged:),
                detail_scroll_indicator_bounds_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(toggleDetailSoftWrap:),
                toggle_detail_soft_wrap as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(detailSaveAs:),
                detail_save_as_action as *mut c_void,
                types.as_ptr(),
            );
            // Main-thread re-entry for save-as: the action only stashes and hops from
            // inside the button tracking loop; the NSSavePanel is presented here (see
            // the PENDING_SAVE_AS comment).
            class_addMethod(
                cls,
                sel!(detailSaveAsDeferred:),
                detail_save_as_deferred as *mut c_void,
                types.as_ptr(),
            );
            // Detail hi-res preview finished (worker -> performSelectorOnMainThread):
            // re-validate freshness, then rebuild the detail panel to upgrade to hi-res.
            class_addMethod(
                cls,
                sel!(detailPreviewReady:),
                detail_preview_ready as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(pickerClose:),
                picker_close as *mut c_void,
                types.as_ptr(),
            );
            // The picker's empty state offers the same action as the settings banner; the panel's
            // buttons target this class, so the selector has to exist here too.
            class_addMethod(
                cls,
                sel!(refreshPermissionUI:),
                refresh_permission_ui as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(retryKeychainAccess:),
                crate::handle_retry_keychain_access as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearClipboardHistory:),
                clear_clipboard_history as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearClipboardAll:),
                clear_clipboard_all as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(expireClipboardUndo:),
                expire_clipboard_undo as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(refreshPickerRows:),
                picker_refresh_rows as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(refreshPickerSearchRows:),
                picker_refresh_search_rows as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(refreshPickerVisibleRows:),
                picker_refresh_visible_rows as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFieldChanged:),
                search_field_changed as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(filterPillClicked:),
                filter_pill_clicked as *mut c_void,
                types.as_ptr(),
            );
            // The detail-text cursor (delivered by the tracking area owned by this
            // observer): enter -> I-beam, exit -> arrow. See detail_tv_cursor_entered.
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                detail_tv_cursor_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                detail_tv_cursor_exited as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFocusBegan:),
                search_focus_began as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(searchFocusEnded:),
                search_focus_ended as *mut c_void,
                types.as_ptr(),
            );
            // Search-field delegate: intercepts commands the field editor translates
            // (e.g. ↓ -> moveDown:).
            let types_cmd = CString::new("B@:@@:").unwrap();
            class_addMethod(
                cls,
                sel!(control:textView:doCommandBySelector:),
                search_field_do_command as *mut c_void,
                types_cmd.as_ptr(),
            );
            objc_registerClassPair(cls);
            // Instance alloc (+1): process-level singleton, never released (matches the
            // static's lifetime).
            let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(obj)
        })
        .0
}

/// Keep the long-lived row view tree current on the main thread; the pasteboard observer only
/// queues one coalesced refresh.
/// Main-thread refresh of the settings window's permission rows (a silent keychain grant produces no
/// activation event, so the About row has to be told).
extern "C" fn refresh_permission_ui(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    crate::settings::refresh_permission_ui_if_visible();
}

extern "C" fn picker_refresh_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    PICKER_REFRESH_PENDING.store(false, Ordering::SeqCst);
    if PICKER_WINDOW.lock().unwrap().is_none() {
        return;
    }
    // While hidden, keep only the model change and refresh before the next presentation so the
    // pasteboard observer cannot occupy the main thread and slow the app switcher.
    if !PICKER_VISIBLE.load(Ordering::SeqCst) {
        with_clipboard_ui(|ui| ui.rendered_rows = None);
        return;
    }

    let filter = *CLIP_FILTER.lock().unwrap();
    let show_source = show_source_app();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let key = picker_rows_key(history_revision(), &query, filter, show_source);
    let rows_current = with_clipboard_ui(|ui| {
        ui.rendered_rows
            .as_ref()
            .is_some_and(|current| current == &key)
    });
    if rows_current {
        return;
    }

    unsafe {
        rebuild_rows();
    }
}

/// Main-thread refresh after coalescing search input: consecutive keystrokes rebuild the
/// visible rows only once after typing pauses.
extern "C" fn picker_refresh_search_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    PICKER_SEARCH_REFRESH_PENDING.store(false, Ordering::SeqCst);
    if !PICKER_VISIBLE.load(Ordering::SeqCst)
        || REBUILDING.load(Ordering::SeqCst)
        || PICKER_WINDOW.lock().unwrap().is_none()
    {
        return;
    }
    let filter = *CLIP_FILTER.lock().unwrap();
    let show_source = show_source_app();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let key = picker_rows_key(history_revision(), &query, filter, show_source);
    let rows_current = with_clipboard_ui(|ui| {
        ui.rendered_rows
            .as_ref()
            .is_some_and(|current| current == &key)
    });
    if !rows_current {
        unsafe {
            rebuild_rows();
        }
    }
}

/// Coalesce consecutive search notifications so the editor stays responsive while the list
/// catches up shortly after typing pauses.
unsafe fn schedule_picker_search_refresh() {
    if PICKER_SEARCH_REFRESH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let target = observer();
    let _: () = msg_send![
        target,
        performSelector: sel!(refreshPickerSearchRows:),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: 0.05f64
    ];
}

/// Materialize the new viewport after a scroll-event burst instead of tearing down and
/// rebuilding the whole physical row set for every bounds-change callback.
extern "C" fn picker_refresh_visible_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    PICKER_VISIBLE_ROWS_REFRESH_PENDING.store(false, Ordering::SeqCst);
    if !PICKER_VISIBLE.load(Ordering::SeqCst) || REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    unsafe {
        if picker_materialized_range_changed() {
            sync_visible_rows();
        }
    }
}

/// Coalesce consecutive scroll notifications, giving AppKit a short run-loop window to finish
/// scrolling before the visible row set is rebuilt.
unsafe fn schedule_picker_visible_rows_refresh() {
    if PICKER_VISIBLE_ROWS_REFRESH_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let target = observer();
    let _: () = msg_send![
        target,
        performSelector: sel!(refreshPickerVisibleRows:),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: 0.05f64
    ];
}

/// Deliver history changes to the main thread while the picker is visible; while hidden, defer
/// the refresh until the next summon.
/// One-time, user-visible notice when this session cannot save the history (no key) or is blocked
/// (data present but unreadable, or a purge unfinished). Shown when the picker opens, because that
/// is where the user looks; a failure that only reaches the log is not reported at all.
/// The one-line "the feature is unavailable right now" copy for a storage state: shown in the
/// settings page, in the opened picker and as the system notification, so all three say the same
/// thing. Pure, unit-tested — a session that cannot obtain the key (or cannot read what is stored)
/// must never look healthy.
pub(super) fn unavailable_copy_key(state: &StorageState) -> Option<&'static str> {
    match state {
        StorageState::Unavailable(_) => Some("clipboard.unavailable_key"),
        StorageState::Blocked(BlockedReason::PurgePending) => Some("clipboard.unavailable_purge"),
        StorageState::Blocked(_) => Some("clipboard.unavailable_history"),
        StorageState::Uninitialized | StorageState::Ready => None,
    }
}

/// The unavailable copy for the current state, resolved in the active locale.
pub(super) fn unavailable_copy() -> Option<String> {
    unavailable_copy_key(&storage::state()).map(t)
}

/// The system notification's title: the feature name, so the banner is identifiable without reading
/// the body.
pub(super) fn unavailable_notification_title() -> String {
    t("clipboard.unavailable_title")
}

/// Post the storage failure as a system notification, once per session. This is the surface that
/// does not depend on the user opening the clipboard panel (the in-picker toast stays as the
/// contextual second chance).
pub(super) fn notify_storage_failure_once() {
    static NOTIFIED: AtomicBool = AtomicBool::new(false);
    let Some(body) = unavailable_copy() else {
        return;
    };
    if NOTIFIED.swap(true, Ordering::SeqCst) {
        return;
    }
    log_info!(
        "[clip] storage failure notification posted (state={}, reason={})",
        storage::state_label(),
        storage::reason_label()
    );
    crate::update_notice::post_local_notice(
        crate::update_notice::ID_CLIPBOARD_STORAGE,
        &unavailable_notification_title(),
        &body,
    );
}

pub(super) fn schedule_picker_refresh() {
    if PICKER_WINDOW.lock().unwrap().is_none()
        || !PICKER_VISIBLE.load(Ordering::SeqCst)
        || PICKER_REFRESH_PENDING.swap(true, Ordering::SeqCst)
    {
        return;
    }
    unsafe {
        let target = observer();
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: sel!(refreshPickerRows:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// Pasteboard-change notification callback (any thread): record the current text immediately.
extern "C" fn pasteboard_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    poll_clipboard();
}

/// Delayed cleanup for a one-shot paste write-back; abort if changeCount or our marker changed.
extern "C" fn clear_system_pasteboard_if_owned(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    let Some(note) = (!note.is_null()).then_some(note as *mut AnyObject) else {
        return;
    };
    let scheduled: i64 = unsafe { msg_send![note, longLongValue] };
    // Each delayed selector carries its own changeCount. An older queued callback must not
    // consume the token belonging to a newer one-shot paste.
    if *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() != Some(scheduled) {
        log_debug!("[clip] system pasteboard clear skipped: stale task");
        return;
    }
    if !clear_system_pasteboard_after_paste() {
        *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = None;
        log_debug!("[clip] system pasteboard clear skipped: setting disabled");
        return;
    }
    unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        let current: i64 = if pb.is_null() {
            -1
        } else {
            msg_send![pb, changeCount]
        };
        let marker_present = !pb.is_null() && pasteboard_has_paste_marker();
        if pb.is_null() || !marker_present || current != scheduled {
            *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = None;
            log_debug!("[clip] system pasteboard clear skipped: ownership changed");
            return;
        }
        let _: isize = msg_send![pb, clearContents];
    }
    *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = None;
    log_debug!("[clip] system pasteboard cleared after one-shot paste");
}

/// Schedule delayed cleanup on the main thread and record the post-write changeCount as the
/// ownership proof.
pub(super) unsafe fn schedule_system_pasteboard_clear() {
    let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
    if pb.is_null() {
        return;
    }
    let cc: i64 = msg_send![pb, changeCount];
    *PENDING_SYSTEM_PASTEBOARD_CLEAR.lock().unwrap() = Some(cc);
    let target = observer();
    let token: *mut AnyObject = msg_send![class!(NSNumber), numberWithLongLong: cc];
    let _: () = msg_send![
        target,
        performSelector: sel!(clearSystemPasteboardIfOwned:),
        withObject: token,
        afterDelay: SYSTEM_PASTEBOARD_CLEAR_DELAY
    ];
}

/// Picker resign-key notification callback (main thread): auto-hide on outside clicks, etc.
extern "C" fn window_did_resign_key(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    hide_picker();
}

pub(super) extern "C" fn clipboard_window_send_event(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut AnyObject,
) {
    unsafe {
        let window = _self as *mut AnyObject;
        type SendEvent = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut AnyObject);
        let mut sup = ObjcSuper {
            receiver: window as *mut c_void,
            super_class: class!(NSPanel) as *const _ as *mut c_void,
        };
        let send_event: SendEvent = std::mem::transmute(objc_msgSendSuper as *const ());
        send_event(&mut sup, sel!(sendEvent:), event);
    }
}

/// The picker and detail share one custom indicator class; only the target scroll view differs.
pub(super) unsafe fn scroll_indicator_class() -> *mut AnyObject {
    static CLASS: OnceLock<usize> = OnceLock::new();
    *CLASS.get_or_init(|| {
        let name = CString::new("OhMyTabClipboardScrollIndicator").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_mouse = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(mouseDown:),
            scroll_indicator_mouse_down as *mut c_void,
            types_mouse.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseDragged:),
            scroll_indicator_mouse_dragged as *mut c_void,
            types_mouse.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseUp:),
            scroll_indicator_mouse_up as *mut c_void,
            types_mouse.as_ptr(),
        );
        let types_accepts_first_mouse = CString::new("B@:@").unwrap();
        class_addMethod(
            cls,
            sel!(acceptsFirstMouse:),
            scroll_indicator_accepts_first_mouse as *mut c_void,
            types_accepts_first_mouse.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls as usize
    }) as *mut AnyObject
}

fn scroll_target_for_indicator(indicator: *mut AnyObject) -> Option<ScrollTarget> {
    if DETAIL_SCROLL_INDICATOR
        .lock()
        .unwrap()
        .is_some_and(|detail| detail.0 == indicator)
    {
        return Some(ScrollTarget::Detail);
    }
    if DETAIL_HORIZONTAL_SCROLL_INDICATOR
        .lock()
        .unwrap()
        .is_some_and(|detail| detail.0 == indicator)
    {
        return Some(ScrollTarget::DetailHorizontal);
    }
    if SCROLL_INDICATOR
        .lock()
        .unwrap()
        .is_some_and(|picker| picker.0 == indicator)
    {
        Some(ScrollTarget::Picker)
    } else {
        None
    }
}

unsafe fn scroll_for_target(target: ScrollTarget) -> Option<*mut AnyObject> {
    match target {
        ScrollTarget::Picker => SCROLL_VIEW.lock().unwrap().map(|scroll| scroll.0),
        ScrollTarget::Detail | ScrollTarget::DetailHorizontal => {
            DETAIL_SCROLL_VIEW.lock().unwrap().map(|scroll| scroll.0)
        }
    }
}

/// Compute the indicator's y/height; dragging and drawing must share this mapping or the
/// thumb jumps when it reaches the end of the track.
pub(super) fn scroll_indicator_geometry(
    visible: f64,
    document: f64,
    offset: f64,
    corner_reserve: f64,
) -> Option<(f64, f64)> {
    // Keep the track end outside the lower-right safe corner; drawing and dragging must use
    // this same track mapping.
    let track_start = SCROLL_INDICATOR_EDGE;
    let track_len = visible - (SCROLL_INDICATOR_EDGE * 2.0) - corner_reserve;
    let max_offset = document - visible;
    let knob_len = crate::scroll_indicator_math::thumb_length(
        track_len,
        visible,
        document,
        SCROLL_INDICATOR_MIN_LEN,
    );
    let placement = crate::scroll_indicator_math::thumb_placement(
        track_start,
        track_len,
        knob_len,
        offset,
        max_offset,
        crate::scroll_indicator_math::ThumbDirection::Forward,
    )?;
    Some((placement.position, knob_len))
}

/// Draw a centered 6pt visible capsule inside the 10pt transparent hit view; the parent view
/// remains responsible for receiving drag events.
pub(super) unsafe fn update_scroll_indicator_visual(
    indicator: *mut AnyObject,
    length: f64,
    horizontal: bool,
) {
    let _: () = msg_send![indicator, setWantsLayer: true];
    let parent_layer: *mut AnyObject = msg_send![indicator, layer];
    let sublayers: *mut AnyObject = msg_send![parent_layer, sublayers];
    let count: usize = if sublayers.is_null() {
        0
    } else {
        msg_send![sublayers, count]
    };
    let visual_layer: *mut AnyObject = if count == 0 {
        let visual: *mut AnyObject = msg_send![class!(CALayer), layer];
        let _: () = msg_send![parent_layer, addSublayer: visual];
        visual
    } else {
        msg_send![sublayers, objectAtIndex: 0usize]
    };
    // Repaint on every update rather than only at creation: the knob has to follow a light/dark
    // switch and this layer outlives it. The colour is a palette token, not a fixed black -- a
    // black knob is invisible against the dark panel.
    let ind_bg = crate::ffi::hex_to_ns_color(crate::theme::ui_palette().scroll_indicator);
    crate::ffi::layer_set_background(visual_layer, crate::ffi::ns_color_to_cg(ind_bg));
    let frame = if horizontal {
        NSRect::new(
            NSPoint::new(0.0, (SCROLL_INDICATOR_HIT_W - SCROLL_INDICATOR_W) / 2.0),
            NSSize::new(length, SCROLL_INDICATOR_W),
        )
    } else {
        NSRect::new(
            NSPoint::new((SCROLL_INDICATOR_HIT_W - SCROLL_INDICATOR_W) / 2.0, 0.0),
            NSSize::new(SCROLL_INDICATOR_W, length),
        )
    };
    let _: () = msg_send![visual_layer, setFrame: frame];
    // CALayer does not clamp a too-large corner radius: RADIUS_FULL would render a square.
    // Cap to half the indicator's bounds so it stays a pill in both orientations.
    let _: () = msg_send![visual_layer, setCornerRadius: crate::theme::rounded_inset_radius(
        crate::theme::RADIUS_FULL,
        frame.size.width,
        frame.size.height,
    )];
}

/// Update the scroll indicator's position/length: shown while the content overflows
/// (always visible, no fade-out), hidden otherwise. Called by the clip-view bounds-change
/// notification callback AND by show_picker (visible on the first summon).
pub(super) unsafe fn update_scroll_indicator_for(target: ScrollTarget) {
    let scroll = match scroll_for_target(target) {
        Some(scroll) => scroll,
        None => return,
    };
    let (indicator, horizontal) = match target {
        ScrollTarget::Picker => (
            match *SCROLL_INDICATOR.lock().unwrap() {
                Some(indicator) => indicator.0,
                None => return,
            },
            false,
        ),
        ScrollTarget::Detail => (
            match *DETAIL_SCROLL_INDICATOR.lock().unwrap() {
                Some(indicator) => indicator.0,
                None => return,
            },
            false,
        ),
        ScrollTarget::DetailHorizontal => (
            match *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() {
                Some(indicator) => indicator.0,
                None => return,
            },
            true,
        ),
    };
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let visible = if horizontal {
        clip_bounds.size.width
    } else {
        clip_bounds.size.height
    };
    let offset = if horizontal {
        clip_bounds.origin.x
    } else {
        clip_bounds.origin.y
    };
    let doc: *mut AnyObject = msg_send![scroll, documentView];
    let document = if doc.is_null() {
        0.0
    } else {
        let df: NSRect = msg_send![doc, frame];
        if horizontal {
            df.size.width
        } else {
            df.size.height
        }
    };

    let corner_reserve = match target {
        ScrollTarget::Picker => 0.0,
        ScrollTarget::Detail | ScrollTarget::DetailHorizontal => SCROLL_INDICATOR_CORNER_RESERVE,
    };
    let Some((knob_pos, knob_len)) =
        scroll_indicator_geometry(visible, document, offset, corner_reserve)
    else {
        let _: () = msg_send![indicator, setHidden: true];
        return;
    };
    let frame = if horizontal {
        NSRect::new(
            NSPoint::new(
                knob_pos,
                clip_bounds.size.height - SCROLL_INDICATOR_HIT_W - 3.0,
            ),
            NSSize::new(knob_len, SCROLL_INDICATOR_HIT_W),
        )
    } else {
        NSRect::new(
            NSPoint::new(
                clip_bounds.size.width - SCROLL_INDICATOR_HIT_W - 3.0,
                knob_pos,
            ),
            NSSize::new(SCROLL_INDICATOR_HIT_W, knob_len),
        )
    };
    let _: () = msg_send![indicator, setFrame: frame];
    update_scroll_indicator_visual(indicator, knob_len, horizontal);
    let _: () = msg_send![indicator, setHidden: false];
}

pub(super) fn update_scroll_indicator() {
    unsafe { update_scroll_indicator_for(ScrollTarget::Picker) }
}

/// Accept the first mouse click so the nonactivating panel can start dragging immediately.
extern "C" fn scroll_indicator_accepts_first_mouse(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) -> bool {
    true
}

/// Convert thumb movement into document offset. The system scroller is disabled, so NSView
/// does not provide this behavior automatically.
extern "C" fn scroll_indicator_mouse_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let Some(target) = scroll_target_for_indicator(_self as *mut AnyObject) else {
            return;
        };
        let scroll = match scroll_for_target(target) {
            Some(scroll) => scroll,
            None => return,
        };
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let clip_bounds: NSRect = msg_send![clip, bounds];
        let doc: *mut AnyObject = msg_send![scroll, documentView];
        if doc.is_null() {
            return;
        }
        let doc_frame: NSRect = msg_send![doc, frame];
        let horizontal = matches!(target, ScrollTarget::DetailHorizontal);
        let visible = if horizontal {
            clip_bounds.size.width
        } else {
            clip_bounds.size.height
        };
        let document = if horizontal {
            doc_frame.size.width
        } else {
            doc_frame.size.height
        };
        let offset = if horizontal {
            clip_bounds.origin.x
        } else {
            clip_bounds.origin.y
        };
        let corner_reserve = match target {
            ScrollTarget::Picker => 0.0,
            ScrollTarget::Detail | ScrollTarget::DetailHorizontal => {
                SCROLL_INDICATOR_CORNER_RESERVE
            }
        };
        let Some((_, knob_len)) =
            scroll_indicator_geometry(visible, document, offset, corner_reserve)
        else {
            return;
        };
        let track_len = visible - (SCROLL_INDICATOR_EDGE * 2.0) - corner_reserve;
        let thumb_travel = track_len - knob_len;
        if thumb_travel <= 0.0 {
            return;
        }
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        let point: NSPoint = msg_send![
            scroll,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let max_offset = (document - visible).max(0.0);
        *SCROLL_DRAG.lock().unwrap() = Some(ScrollDragState {
            target,
            start_axis: if horizontal { point.x } else { point.y },
            start_offset: offset.clamp(0.0, max_offset),
            max_offset,
            thumb_travel,
        });
    }
}

extern "C" fn scroll_indicator_mouse_dragged(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let drag = match *SCROLL_DRAG.lock().unwrap() {
            Some(drag) => drag,
            None => return,
        };
        let scroll = match scroll_for_target(drag.target) {
            Some(scroll) => scroll,
            None => return,
        };
        let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
        let point: NSPoint = msg_send![
            scroll,
            convertPoint: location,
            fromView: std::ptr::null::<AnyObject>()
        ];
        let horizontal = matches!(drag.target, ScrollTarget::DetailHorizontal);
        let axis = if horizontal { point.x } else { point.y };
        let offset = crate::scroll_indicator_math::drag_offset(
            drag.start_offset,
            drag.start_axis,
            axis,
            drag.max_offset,
            drag.thumb_travel,
            crate::scroll_indicator_math::ThumbDirection::Forward,
        );
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let bounds: NSRect = msg_send![clip, bounds];
        let origin = if horizontal {
            NSPoint::new(offset, bounds.origin.y)
        } else {
            NSPoint::new(bounds.origin.x, offset)
        };
        let _: () = msg_send![clip, setBoundsOrigin: origin];
        let _: () = msg_send![scroll, reflectScrolledClipView: clip];
    }
}

extern "C" fn scroll_indicator_mouse_up(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    *SCROLL_DRAG.lock().unwrap() = None;
}

/// Clip-view bounds-change notification callback (scrolling) -> update the indicator; with
/// the detail open, move it along so it keeps following the selected row (otherwise a
/// scroll would leave the detail misaligned with its row).
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
extern "C" fn scroll_indicator_bounds_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    crate::callback_guard::void("scroll_indicator_bounds_changed", || unsafe {
        scroll_indicator_bounds_changed_inner(_self, _cmd, _note)
    });
}

unsafe fn scroll_indicator_bounds_changed_inner(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    update_scroll_indicator();
    if !REBUILDING.load(Ordering::SeqCst) && PICKER_VISIBLE.load(Ordering::SeqCst) {
        unsafe {
            if picker_materialized_range_changed() {
                schedule_picker_visible_rows_refresh();
            }
        }
    }
    reposition_detail();
}

/// Check whether a row intersects the viewport, including overscan.
pub(super) fn picker_row_is_drawable(row: NSRect, viewport: NSRect, overscan: f64) -> bool {
    row.origin.y + row.size.height >= viewport.origin.y - overscan
        && row.origin.y <= viewport.origin.y + viewport.size.height + overscan
}

/// Compute the materialized display-index range from all row heights, retaining a small
/// overscan on both sides to avoid flashing at scroll boundaries.
pub(super) fn picker_visible_range(
    pitches: &[f64],
    viewport: NSRect,
    overscan: f64,
) -> (usize, usize) {
    if pitches.is_empty() || viewport.size.height <= 0.0 {
        return (0, 0);
    }
    // One-pass prefix sums give every row top: calling `row_top` per row re-sums the
    // preceding pitches, degrading the scan to O(n^2) (the same trap rebuild_rows fixed;
    // see model::row_offsets).
    let offsets = row_offsets(pitches);
    let mut start = None;
    let mut end = 0;
    for (index, &height) in pitches.iter().enumerate() {
        let row = NSRect::new(
            NSPoint::new(0.0, offsets[index]),
            NSSize::new(PICKER_W, height),
        );
        if picker_row_is_drawable(row, viewport, overscan) {
            start.get_or_insert(index);
            end = index + 1;
        }
    }
    match start {
        Some(start) => (start, end),
        None => (0, pitches.len().min(1)),
    }
}

/// Read the current list viewport; before layout completes, warm only a small top slice so
/// the first presentation never creates the entire list synchronously.
pub(super) unsafe fn picker_visible_row_range(pitches: &[f64], row_count: usize) -> (usize, usize) {
    if row_count == 0 {
        return (0, 0);
    }
    let fallback_end = row_count.min(12);
    let Some(container) = picker_container_ptr() else {
        return (0, fallback_end);
    };
    let Some(scroll) = *SCROLL_VIEW.lock().unwrap() else {
        return (0, fallback_end);
    };
    let clip: *mut AnyObject = msg_send![scroll.0, contentView];
    if clip.is_null() {
        return (0, fallback_end);
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let visible_rect: NSRect = msg_send![
        container,
        convertRect: clip_bounds,
        fromView: clip
    ];
    if visible_rect.size.width <= 0.0 || visible_rect.size.height <= 0.0 {
        return (0, fallback_end);
    }
    picker_visible_range(pitches, visible_rect, ROW_H * 1.5)
}

/// Check whether the current physical row slots cover the range needed by the viewport.
pub(super) unsafe fn picker_materialized_range_changed() -> bool {
    let pitches = ROW_PITCHES.lock().unwrap().clone();
    let filtered_len = with_clipboard_ui(|ui| ui.filtered.len());
    let (start, end) = picker_visible_row_range(&pitches, filtered_len);
    let indices = ROW_VIEW_INDICES.lock().unwrap();
    !indices.iter().copied().eq(start..end)
}

pub(super) fn row_view_for_display_index(index: usize) -> Option<RowHoverViews> {
    let indices = ROW_VIEW_INDICES.lock().unwrap();
    let slot = indices
        .iter()
        .position(|&display_index| display_index == index)?;
    ROW_HOVER_VIEWS.lock().unwrap().get(slot).copied()
}

/// Return the detail NSClipView's live legal vertical range. NSTextView's text-container inset
/// means the endpoints are not necessarily `0..documentHeight-visibleHeight`; AppKit must
/// constrain them.
pub(super) unsafe fn detail_scroll_range(scroll: *mut AnyObject) -> Option<(f64, f64)> {
    if scroll.is_null() {
        return None;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    if clip.is_null() {
        return None;
    }
    let bounds: NSRect = msg_send![clip, bounds];
    let top_request = NSRect::new(NSPoint::new(bounds.origin.x, -1_000_000_000.0), bounds.size);
    let bottom_request = NSRect::new(NSPoint::new(bounds.origin.x, 1_000_000_000.0), bounds.size);
    let top: NSRect = msg_send![clip, constrainBoundsRect: top_request];
    let bottom: NSRect = msg_send![clip, constrainBoundsRect: bottom_request];
    Some((
        top.origin.y.min(bottom.origin.y),
        top.origin.y.max(bottom.origin.y),
    ))
}

/// Scroll unconditionally to AppKit's actual constrained top instead of assuming y=0.
pub(super) unsafe fn scroll_detail_to_top(scroll: *mut AnyObject) {
    let Some((min_y, _)) = detail_scroll_range(scroll) else {
        return;
    };
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let bounds: NSRect = msg_send![clip, bounds];
    let _: () = msg_send![
        clip,
        setBoundsOrigin: NSPoint::new(bounds.origin.x, min_y)
    ];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
}

/// Bounds changes from the detail's native scroll view -> update the custom capsule
/// indicators only. Endpoint overscroll (rubber banding) is native elasticity's job and
/// must never be answered by rewriting the clip-view bounds here -- a synchronous hard
/// clamp mid-gesture poisons NSScrollView's momentum base, so each following momentum
/// event reapplies its delta from the stale base and the two systems fight until the decay
/// ends (the on-screen scrollbar twitch). During rubber banding the indicator geometry
/// clamps progress to 0..1, so the thumb pins at the endpoint just like a system scroller.
pub(super) extern "C" fn detail_scroll_indicator_bounds_changed(
    _self: *mut c_void,
    _cmd: Sel,
    _note: *mut c_void,
) {
    // update_scroll_indicator_for reads DETAIL_SCROLL_VIEW itself and returns silently
    // when the view is gone.
    unsafe {
        update_scroll_indicator_for(ScrollTarget::Detail);
        update_scroll_indicator_for(ScrollTarget::DetailHorizontal);
    }
}
pub(super) fn cancel_clipboard_undo_timer() {
    unsafe {
        let target = observer();
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: target,
            selector: sel!(expireClipboardUndo:),
            object: std::ptr::null::<AnyObject>()
        ];
    }
}

pub(super) fn discard_deleted_clipboard_entry() {
    let pending = DELETED_CLIPBOARD_ENTRY.lock().unwrap().take();
    let Some(pending) = pending else {
        return;
    };
    let history = CLIP_HISTORY.lock().unwrap();
    cache_delete_for_removed(&history, &pending.entry);
}

fn schedule_clipboard_undo_expiry() {
    cancel_clipboard_undo_timer();
    unsafe {
        let target = observer();
        let _: () = msg_send![
            target,
            performSelector: sel!(expireClipboardUndo:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: CLIPBOARD_UNDO_WINDOW.as_secs_f64()
        ];
    }
}

pub(super) fn remember_deleted_clipboard_entry(entry: ClipEntry, original_index: usize) {
    discard_deleted_clipboard_entry();
    let generation = DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    *DELETED_CLIPBOARD_ENTRY.lock().unwrap() = Some(DeletedClipboardEntry {
        entry,
        original_index,
        expires_at: Instant::now() + CLIPBOARD_UNDO_WINDOW,
        generation,
    });
    schedule_clipboard_undo_expiry();
}

fn expire_deleted_clipboard_entry() {
    let generation = DELETED_CLIPBOARD_GENERATION.load(Ordering::Acquire);
    let expired = DELETED_CLIPBOARD_ENTRY
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|pending| {
            pending.generation == generation
                && clipboard_undo_expired(pending.expires_at, Instant::now())
        });
    if expired {
        discard_deleted_clipboard_entry();
        DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    }
}

extern "C" fn expire_clipboard_undo(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    expire_deleted_clipboard_entry();
}

pub(super) fn undo_deleted_clipboard_entry() -> Option<ClipEntry> {
    let pending = DELETED_CLIPBOARD_ENTRY.lock().unwrap().take()?;
    cancel_clipboard_undo_timer();
    if clipboard_undo_expired(pending.expires_at, Instant::now()) {
        let history = CLIP_HISTORY.lock().unwrap();
        cache_delete_for_removed(&history, &pending.entry);
        return None;
    }
    let restored = pending.entry.clone();
    let mut history = CLIP_HISTORY.lock().unwrap();
    let (_, inserted) = restore_entry_at(&mut history, pending.entry, pending.original_index);
    let max = max_entries();
    if history.len() > max {
        let dropped: Vec<ClipEntry> = history.drain(max..).collect();
        for entry in &dropped {
            cache_delete_for_removed(&history, entry);
        }
    }
    let restored_h_idx = history
        .iter()
        .position(|entry| same_clip_entry_identity(entry, &restored));
    let present = restored_h_idx.is_some();
    if let Some(h_idx) = restored_h_idx {
        let query = with_clipboard_ui(|ui| ui.search_query.clone());
        let filter = *CLIP_FILTER.lock().unwrap();
        let filtered = filtered_indices(&history, &query, filter);
        if let Some(display_idx) = filtered.iter().position(|&idx| idx == h_idx) {
            set_picker_selection(display_idx);
        }
    }
    drop(history);
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    if inserted || present {
        save_history();
    }
    present.then_some(restored)
}

pub(super) fn picker_filters_y() -> f64 {
    TOP_PAD_Y + SEARCH_H + SEARCH_GAP_Y
}

extern "C" fn clear_clipboard_history(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    clear_clipboard_history_scope(false);
}

extern "C" fn clear_clipboard_all(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    clear_clipboard_history_scope(true);
}

/// The header's corner close button: the same dismissal as Esc with no query / an outside click.
extern "C" fn picker_close(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    hide_picker();
}

/// Clear the visible history scope (see `remove_history_scope`) and release the cache files of the
/// entries that went away, then rewrite the file and refresh the list. The plain clear keeps the
/// pinned entries; only the explicit "clear all" button takes them.
fn clear_clipboard_history_scope(clear_all: bool) {
    discard_deleted_clipboard_entry();
    cancel_clipboard_undo_timer();
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    // The scope is what the user is looking at: the active filter narrowed by the search query. The
    // query stays in the field -- the list then shows whatever is left of it, which is the honest
    // result of clearing the visible set.
    let filter = *CLIP_FILTER.lock().unwrap();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let mut history = CLIP_HISTORY.lock().unwrap();
    let removed_entries = remove_history_scope(&mut history, clear_all, filter, &query);
    let removed_hashes: HashSet<u64> = removed_entries
        .iter()
        .filter_map(|entry| entry.image.as_ref().map(|image| image.hash))
        .filter(|hash| *hash != 0)
        .collect();
    for hash in removed_hashes {
        cache_delete_for_hash(&history, hash);
    }
    let kept_count = history.len();
    drop(history);
    save_history();
    hide_detail();
    unsafe { rebuild_rows() };
    log_info!(
        "Clipboard history cleared by user (clear_all={}, filter={:?}, query={}, kept_entries={})",
        clear_all,
        filter as u8,
        !query.is_empty(),
        kept_count
    );
}

/// Clear the search query and the search field's text (no rebuild; callers rebuild as needed).
unsafe fn set_search_clear_button_visible(visible: bool) {
    if let Some(button) = *SEARCH_CLEAR_BUTTON.lock().unwrap() {
        let _: () = msg_send![button.0, setHidden: !visible];
    }
}

pub(super) fn clear_search() {
    with_clipboard_ui(|ui| ui.search_query.clear());
    SEARCH_CLEAR_HOVERED.store(false, Ordering::SeqCst);
    unsafe { set_search_clear_button_visible(false) };
    // The clear actions name the scope they clear, which is no longer "current view".
    update_clear_action_labels();
    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
        unsafe {
            let empty_ns = make_nsstring("");
            let _: () = msg_send![f.0, setStringValue: empty_ns];
            let _: () = msg_send![f.0, setNeedsDisplay: true];
            CFRelease(empty_ns as *const c_void);
        }
    }
}

/// Search-field text-change notification callback: update the query and rebuild the filter.
extern "C" fn search_field_changed(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    let field: *mut AnyObject = unsafe { msg_send![note as *mut AnyObject, object] };
    if field.is_null() {
        return;
    }
    let s: *mut AnyObject = unsafe { msg_send![field, stringValue] };
    let q = unsafe { nsstring_to_rust(s) };
    let has_query = !q.is_empty();
    with_clipboard_ui(|ui| ui.search_query = q);
    if !has_query {
        SEARCH_CLEAR_HOVERED.store(false, Ordering::SeqCst);
    }
    unsafe { set_search_clear_button_visible(has_query) };
    // The clear actions name their scope, which now includes "results" instead of a category.
    update_clear_action_labels();
    // The hand-drawn ⌘F keycap and right × are positioned from query state, so force a redraw
    // whenever text changes.
    unsafe {
        let _: () = msg_send![field, setNeedsDisplay: true];
    }
    // Do NOT reset the selection: while editing (focus in the search field) it stays
    // "no selection"; returning to the list (↓) resets it to the first entry in
    // search_field_do_command.
    unsafe { schedule_picker_search_refresh() };
}

/// NSSearchField's Esc (cancelOperation:): a query gets cleared and the full list restored
/// (scheme A, level one); with no query the picker closes (level two).
pub(super) extern "C" fn search_field_cancel(_self: *mut c_void, _cmd: Sel) {
    let has_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
    if has_query {
        clear_search();
        // Focus stays in the search field: keep "no selection" (no highlight returns).
        unsafe { rebuild_rows() };
    } else {
        hide_picker();
    }
}

/// The search field's right clear button: do not route through responder-chain cancelOperation:
/// (which the field editor may intercept); clear the field and filter directly. Unlike Esc,
/// clicking an already-empty field never closes the picker.
pub(super) extern "C" fn search_clear_button(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    clear_search();
    unsafe { rebuild_rows() };
}

/// Hit-tests the custom search ×: intercept only the rightmost 18pt while queried and forward
/// every other mouse event to the superclass normally.
/// Whether a mouse location falls inside the search field's custom right-side × hit area.
unsafe fn search_clear_contains_event(field: *mut AnyObject, event: *mut c_void) -> bool {
    if !search_has_query() {
        return false;
    }
    let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
    let point: NSPoint =
        msg_send![field, convertPoint: location, fromView: std::ptr::null::<AnyObject>()];
    let bounds: NSRect = msg_send![field, bounds];
    point.x >= bounds.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W
        && point.x <= bounds.size.width - SEARCH_PAD_IN
        && point.y >= 0.0
        && point.y <= bounds.size.height
}

/// Refreshes the custom × hover state; redraws only the search field on changes, never filters
/// or rebuilds the list.
unsafe fn update_search_clear_hover(field: *mut AnyObject, event: *mut c_void) {
    let hovered = search_clear_contains_event(field, event);
    if SEARCH_CLEAR_HOVERED.swap(hovered, Ordering::SeqCst) != hovered {
        let _: () = msg_send![field, setNeedsDisplay: true];
    }
}

pub(super) extern "C" fn search_field_mouse_moved(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe { update_search_clear_hover(_self as *mut AnyObject, event) }
}

pub(super) extern "C" fn search_field_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe { update_search_clear_hover(_self as *mut AnyObject, event) }
}

pub(super) extern "C" fn search_field_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    if SEARCH_CLEAR_HOVERED.swap(false, Ordering::SeqCst) {
        unsafe {
            let field = _self as *mut AnyObject;
            let _: () = msg_send![field, setNeedsDisplay: true];
        }
    }
}

pub(super) extern "C" fn search_field_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    unsafe {
        let field = _self as *mut AnyObject;
        if search_clear_contains_event(field, event) {
            search_clear_button(_self, sel!(clearSearch:), event);
            return;
        }
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void) -> ();
        let super_class = class!(NSSearchField) as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class,
        };
        let f: F = std::mem::transmute(objc_msgSendSuper as *const ());
        f(&mut sup, sel!(mouseDown:), event);
    }
}

/// The search field's fill/ring helper (raw FFI for the layer background). Focus strengthens
/// only the inner ring and keeps the frosted fill stable.
pub(super) unsafe fn style_search_field(field: *mut AnyObject, focused: bool) {
    let layer: *mut AnyObject = msg_send![field, layer];
    let palette = clipboard_palette();
    let background = crate::ffi::hex_to_cg_color(palette.field_bg);
    crate::ffi::layer_set_background(layer, background);
    let ring = if focused {
        palette.accent
    } else {
        palette.card_border
    };
    crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(ring));
}

/// Editing begins: keep the default 4.5% frosted fill and use only a 10% inner ring for focus,
/// avoiding a disruptive white transition while typing.
extern "C" fn search_focus_began(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    unsafe {
        let field: *mut AnyObject = msg_send![note as *mut AnyObject, object];
        if !field.is_null() {
            style_search_field(field, true);
            // Focus transitions must explicitly redraw the cell: the placeholder is
            // cell-drawn and hides on focus (during IME composition stringValue stays
            // empty, so a stale placeholder would sit under the pre-edit pinyin).
            // Layer background changes do not trigger cell redraws.
            let _: () = msg_send![field, setNeedsDisplay: true];
        }
    }
}

/// Editing ends: restore the default inner ring.
extern "C" fn search_focus_ended(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    unsafe {
        let field: *mut AnyObject = msg_send![note as *mut AnyObject, object];
        if !field.is_null() {
            style_search_field(field, false);
            // Same as search_focus_began: restoring the placeholder on blur needs an
            // immediate redraw.
            let _: () = msg_send![field, setNeedsDisplay: true];
        }
    }
}

/// Whether the materialized rows already match the current history/query/filter (the same
/// predicate used by the refresh callbacks).
fn picker_rows_are_current() -> bool {
    let filter = *CLIP_FILTER.lock().unwrap();
    let show_source = show_source_app();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let key = picker_rows_key(history_revision(), &query, filter, show_source);
    with_clipboard_ui(|ui| {
        ui.rendered_rows
            .as_ref()
            .is_some_and(|current| current == &key)
    })
}

/// Search-field delegate command interception: ↓ (moveDown:) moves focus into the list and
/// selects the first filtered entry, returning YES (consumed); any other command returns NO
/// so the field editor handles it (cursor movement / text input).
///
/// Why this is necessary: once the search field edits, the FIRST RESPONDER is the window's
/// field editor (an NSTextView) -- key events never reach the search field's keyDown:. The
/// editor translates ↓ into a moveDown: command and forwards it to the field's delegate via
/// control:textView:doCommandBySelector: -- the official way to intercept keys on text controls.
pub(super) extern "C" fn search_field_do_command(
    _self: *mut c_void,
    _cmd: Sel,
    _control: *mut c_void,
    _text_view: *mut c_void,
    command_selector: Sel,
) -> bool {
    if command_selector != sel!(moveDown:) && command_selector != sel!(moveUp:) {
        return false;
    }
    unsafe {
        // The query/filter stays; only focus and the selection move to the list.
        // ↓ = the newest entry (first row); ↑ = the oldest (the display list's tail,
        // scrolled into view afterwards).
        let display_len = with_clipboard_ui(|ui| ui.filtered.len());
        let sel = if command_selector == sel!(moveUp:) {
            // Empty list: 0 (no row to select, no highlight; saturating_sub guards).
            display_len.saturating_sub(1)
        } else {
            0
        };
        let previous = picker_selection();
        set_picker_selection(sel);
        // When the rows already match the query, update only the two rows' highlights; right
        // after a query change (the debounced refresh has not landed yet) a full rebuild is
        // still required, otherwise the new list index is applied to stale rows.
        if picker_rows_are_current() {
            refresh_selection(previous, sel);
        } else {
            rebuild_rows();
        }
        // With the detail open, its filled action icon must follow the new selection.
        if detail_visible() {
            refresh_detail_action_visuals();
        }
        // With ↑ the tail is selected while the viewport is still at the top: use the
        // deterministic offset calculation to bring the selected row into view.
        if let Some(container) = picker_container_ptr() {
            scroll_selection_into_view(container, sel);
        }
        if let Some(container) = picker_container_ptr() {
            let window = match *PICKER_WINDOW.lock().unwrap() {
                Some(w) => w.0,
                None => return true,
            };
            // makeFirstResponder: returns BOOL ('B').
            let _: bool = msg_send![window, makeFirstResponder: container];
        }
    }
    true
}

/// Whether the pasteboard-change notification has been registered (idempotent; start/stop
/// cycles must not double-register and duplicate callbacks).
static NOTIFICATION_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Register the pasteboard-change notification (once).
pub(super) unsafe fn register_pasteboard_observer() {
    if NOTIFICATION_REGISTERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let name = make_nsstring("NSPasteboardDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardPasteboardChanged:),
        name: name,
        object: std::ptr::null::<AnyObject>()
    ];
    CFRelease(name as *const c_void);
    log_debug!("Pasteboard change observer registered.");
}

/// The NSTimer target: NSTimer sends clipPollTick: to it. A tiny dynamic class forwards the
/// method to clip_poll_tick; the class is registered once, and an instance is created per start.
pub(super) unsafe fn timer_target() -> *mut AnyObject {
    static TIMER_CLS: OnceLock<StaticClass> = OnceLock::new();
    let cls = *TIMER_CLS.get_or_init(|| {
        let name = CString::new("OhMyTabClipTimerTarget").unwrap();
        let superclass = class!(NSObject) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(clipPollTick:),
            clip_poll_tick as *mut c_void,
            types.as_ptr(),
        );
        objc_registerClassPair(cls);
        StaticClass(cls as *const objc2::runtime::AnyClass)
    });
    let obj: *mut AnyObject = msg_send![cls.0 as *const AnyObject, new];
    obj
}
