//! Clipboard subsystem · notifications: pasteboard notification observers.

use super::*;
use crate::callback_guard;

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
                sel!(clipboardWindowBecameKey:),
                window_did_become_key as *mut c_void,
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
            // The header's settings button opens the settings window on the clipboard page.
            class_addMethod(
                cls,
                sel!(openClipboardSettings:),
                open_clipboard_settings as *mut c_void,
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
                sel!(clearClipboardStorage:),
                crate::handle_clear_clipboard_storage as *mut c_void,
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
                sel!(togglePanelPin:),
                toggle_panel_pin as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(togglePauseRecording:),
                toggle_pause_recording as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clipboardOutsideClick:),
                clipboard_outside_click as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clipboardPasteHandbackTick:),
                super::paste_handback::paste_handback_tick as *mut c_void,
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
                sel!(toggleFilterPopover:),
                toggle_filter_popover as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(clearDayGroup:),
                clear_day_group as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(popoverFilterSelected:),
                popover_filter_selected as *mut c_void,
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
    callback_guard::void("refresh_permission_ui", || {
        crate::settings::refresh_permission_ui_if_visible();
    });
}

extern "C" fn picker_refresh_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("picker_refresh_rows", || {
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
    });
}

/// Main-thread refresh after coalescing search input: consecutive keystrokes rebuild the
/// visible rows only once after typing pauses.
extern "C" fn picker_refresh_search_rows(_self: *mut c_void, _cmd: Sel, _arg: *mut c_void) {
    callback_guard::void("picker_refresh_search_rows", || {
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
    });
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
    callback_guard::void("picker_refresh_visible_rows", || {
        PICKER_VISIBLE_ROWS_REFRESH_PENDING.store(false, Ordering::SeqCst);
        if !PICKER_VISIBLE.load(Ordering::SeqCst) || REBUILDING.load(Ordering::SeqCst) {
            return;
        }
        unsafe {
            if picker_materialized_range_changed() {
                sync_visible_rows();
            }
        }
    });
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
        // A migration or damage can clear by itself (a permission or IO obstruction that goes away),
        // so this copy says a restart retries. A foreign key needs the matching key back and a newer
        // format needs a newer build, so those keep the plain sentence: suggesting a restart there
        // would promise something the same version cannot deliver.
        StorageState::Blocked(BlockedReason::MigrationIncomplete | BlockedReason::Damaged) => {
            Some("clipboard.unavailable_history_retry")
        }
        StorageState::Blocked(_) => Some("clipboard.unavailable_history"),
        StorageState::Uninitialized | StorageState::Ready => None,
    }
}

/// What a storage notice may offer the user. Pure: which action a state admits is a rule about the
/// state machine, not about any one view (plan §4.5/§4.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StorageAction {
    /// Re-read the keychain key and re-run the load. The only action that can clear `Unavailable`.
    GrantKeychain,
    /// Delete the storage set through the purge transaction and start a fresh history. The only way
    /// out of a `Blocked` state: re-reading the key cannot change what is on disk.
    ClearAndRestart,
    /// Nothing to offer. A pending purge already retries on every launch and config change, and the
    /// healthy states need no action at all.
    None,
}

/// The action a state admits (see `StorageAction`).
pub(super) fn storage_action_key(state: &StorageState) -> StorageAction {
    match state {
        StorageState::Unavailable(_) => StorageAction::GrantKeychain,
        StorageState::Blocked(
            BlockedReason::MigrationIncomplete
            | BlockedReason::Damaged
            | BlockedReason::ForeignKey
            | BlockedReason::UnsupportedVersion,
        ) => StorageAction::ClearAndRestart,
        StorageState::Blocked(BlockedReason::PurgePending)
        | StorageState::Uninitialized
        | StorageState::Ready => StorageAction::None,
    }
}

/// The action for the current state.
pub(crate) fn storage_action() -> StorageAction {
    storage_action_key(&storage::state())
}

/// What the About page's keychain row reports, and whether it offers the keychain action.
///
/// `Uninitialized` must not read as "needs authorization": right after a purge (or before the first
/// load lands) the session has simply not asked for the key yet, and `Blocked` is not a keychain
/// problem at all — the keychain action there would be a dead end (see `StorageAction`).
///
/// A held key answers the keychain question whatever the storage state is: the row reports
/// AUTHORIZATION, not writability. `acquire_key_only` (this row's own action while the feature is
/// off) holds a key without loading anything, so reporting "not read yet" after a successful grant
/// would be wrong in exactly the way this row exists to avoid.
pub(super) fn keychain_row_status_key(
    state: &StorageState,
    key_held: bool,
) -> (&'static str, bool) {
    match state {
        _ if key_held => ("settings.keychain_status_granted", false),
        StorageState::Unavailable(_) => ("settings.keychain_status_missing", true),
        StorageState::Blocked(BlockedReason::PurgePending) => {
            ("settings.keychain_status_cleanup", false)
        }
        StorageState::Blocked(_) => ("settings.keychain_status_unreadable", false),
        StorageState::Uninitialized | StorageState::Ready => {
            ("settings.keychain_status_not_read", false)
        }
    }
}

/// The About page's keychain row for the current state.
pub(crate) fn keychain_row_status() -> (&'static str, bool) {
    keychain_row_status_key(&storage::state(), storage::key().is_some())
}

/// A surface that reports the storage state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NoticeSurface {
    SettingsBanner,
    PickerEmptyState,
    AboutRow,
}

/// What a notice surface actually put on screen. `Absent` (no such view in this process) and
/// `Hidden` (the view exists with its action hidden) are different facts — a state field that
/// conflated them could not tell a missing surface from a hidden button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AppliedAction {
    /// The view does not exist in this process (never created, or no window open).
    Absent,
    /// The view exists and carries no action (hidden, or a state that offers none).
    Hidden,
    /// The view carries the keychain action.
    Grant,
    /// The view carries the clear-and-restart action.
    Clear,
}

impl AppliedAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Hidden => "hidden",
            Self::Grant => "grant",
            Self::Clear => "clear",
        }
    }
}

/// The last APPLIED notice state per surface, for `--e2e-state`: what the UI actually put on screen,
/// not what the classifier would allow. `Unset` means nothing has been applied yet, which is also
/// not the same fact as `Absent`.
#[derive(Clone, Copy)]
struct AppliedNotice {
    banner: Option<AppliedAction>,
    panel: Option<AppliedAction>,
    about: Option<AppliedAction>,
    about_status: Option<&'static str>,
}

impl AppliedNotice {
    const fn new() -> Self {
        Self {
            banner: None,
            panel: None,
            about: None,
            about_status: None,
        }
    }
}

static APPLIED_NOTICE: Mutex<AppliedNotice> = Mutex::new(AppliedNotice::new());

/// Record what a surface applied (called from the UI paths themselves, on the main thread).
pub(crate) fn record_applied_action(surface: NoticeSurface, action: AppliedAction) {
    let mut applied = APPLIED_NOTICE.lock().unwrap();
    match surface {
        NoticeSurface::SettingsBanner => applied.banner = Some(action),
        NoticeSurface::PickerEmptyState => applied.panel = Some(action),
        NoticeSurface::AboutRow => applied.about = Some(action),
    }
}

/// Record the About row's applied status (the short suffix of its `settings.keychain_status_*` key).
pub(crate) fn record_applied_about_status(status: &'static str) {
    APPLIED_NOTICE.lock().unwrap().about_status = Some(status);
}

/// The applied labels for `--e2e-state`: (banner, panel, about row, about row status).
pub(crate) fn applied_notice_labels() -> (&'static str, &'static str, &'static str, &'static str) {
    let applied = *APPLIED_NOTICE.lock().unwrap();
    let label = |action: Option<AppliedAction>| action.map_or("unset", AppliedAction::label);
    (
        label(applied.banner),
        label(applied.panel),
        label(applied.about),
        applied.about_status.unwrap_or("unset"),
    )
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
    callback_guard::void("pasteboard_changed", || {
        poll_clipboard();
    });
}

/// Delayed cleanup for a one-shot paste write-back; abort if changeCount or our marker changed.
extern "C" fn clear_system_pasteboard_if_owned(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    callback_guard::void("clear_system_pasteboard_if_owned", || {
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
    });
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

// ---------------------------------------------------------------------------------------------
// Outside-click monitor (decision C: an unpinned, visible, non-key panel closes on the next click)
// ---------------------------------------------------------------------------------------------

/// The pinned collection behavior: join every Space, participate in other apps' fullscreen and
/// Stage Manager, and stay out of window cycling. Transient means Mission Control does not
/// enumerate it - a temporary display state, never a close or an unpin.
pub(super) const PINNED_COLLECTION_BEHAVIOR: usize = (1 << 0)  // CanJoinAllSpaces
    | (1 << 3)  // Transient
    | (1 << 6)  // IgnoresCycle
    | (1 << 8)  // FullScreenAuxiliary
    | (1 << 18); // CanJoinAllApplications (macOS 13+)

struct ClickMonitor {
    local: *mut AnyObject,
    global: *mut AnyObject,
    /// The monitor borrows the block pointer rather than retaining it, so it is kept alive here
    /// (same ownership discipline as the settings dropdown monitor).
    #[allow(dead_code)]
    handler: block2::RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject>,
}

unsafe impl Send for ClickMonitor {}
unsafe impl Sync for ClickMonitor {}

static CLICK_MONITOR: Mutex<Option<ClickMonitor>> = Mutex::new(None);

/// Install the click monitors when the panel is visible and remove them when it is not.
///
/// A global monitor sees only OTHER applications' events, so a click in one of our own windows
/// (the settings window, the detail panel) is invisible to it; the local monitor covers those. Both
/// are needed, and mouse monitoring needs no Accessibility grant (that restriction is for key
/// events). The monitor is NOT tied to the pin: it must stay installed across a pin flip, because
/// the state that decides whether a click dismisses can change without a new showing.
/// Returns whether the panel is guarded (always true when hidden, since nothing needs guarding).
pub(super) unsafe fn sync_outside_click_monitor() -> bool {
    if super::picker_is_visible() {
        install_outside_click_monitor()
    } else {
        remove_outside_click_monitor();
        true
    }
}

/// Install both monitors. Returns false when the pair cannot be installed, in which case whatever
/// did install is removed again - a half-installed pair cannot honour the click-outside contract.
unsafe fn install_outside_click_monitor() -> bool {
    if CLICK_MONITOR.lock().unwrap().is_some() {
        return true;
    }
    let handler: block2::RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject> =
        block2::RcBlock::new(|event: *mut AnyObject| -> *mut AnyObject {
            handle_outside_click_event(event);
            // The local handler must return the event unchanged: swallowing it would break the
            // target application or our own settings window.
            event
        });
    // LeftMouseDown(1) | RightMouseDown(3) | OtherMouseDown(25); the mask is 1 << type.
    let mask: u64 = (1u64 << 1) | (1u64 << 3) | (1u64 << 25);
    let local: *mut AnyObject = msg_send![
        class!(NSEvent),
        addLocalMonitorForEventsMatchingMask: mask,
        handler: &*handler
    ];
    let global: *mut AnyObject = msg_send![
        class!(NSEvent),
        addGlobalMonitorForEventsMatchingMask: mask,
        handler: &*handler
    ];
    // A half-installed pair cannot honour decision C, so refuse the pin rather than pretend: the
    // ordinary panel keeps working, and the state is reported and retryable.
    if local.is_null() || global.is_null() {
        if !local.is_null() {
            let _: () = msg_send![class!(NSEvent), removeMonitor: local];
        }
        if !global.is_null() {
            let _: () = msg_send![class!(NSEvent), removeMonitor: global];
        }
        log_info!("[clip] outside-click monitor unavailable; refusing to pin");
        return false;
    }
    *CLICK_MONITOR.lock().unwrap() = Some(ClickMonitor {
        local,
        global,
        handler,
    });
    true
}

unsafe fn remove_outside_click_monitor() {
    let taken = CLICK_MONITOR.lock().unwrap().take();
    if let Some(installed) = taken {
        let _: () = msg_send![class!(NSEvent), removeMonitor: installed.local];
        let _: () = msg_send![class!(NSEvent), removeMonitor: installed.global];
    }
}

/// Decide what an outside click means, from the state on screen right now.
///
/// Only three outcomes matter: a pinned panel ignores it; an unpinned panel that is still key lets
/// AppKit's own resign-key path do the dismissal (this monitor must not race it); and an unpinned,
/// visible, NON-key panel is the state decision C creates, where no second resign-key will ever
/// arrive and this callback is the only thing that can close it.
fn handle_outside_click_event(event: *mut AnyObject) {
    unsafe {
        if super::is_pinned() || !super::picker_is_visible() {
            return;
        }
        let Some(window) = *super::PICKER_WINDOW.lock().unwrap() else {
            return;
        };
        let is_key: bool = msg_send![window.0, isKeyWindow];
        if is_key {
            return;
        }
        if let Some(point) = event_screen_location(event) {
            if panel_group_contains(point) {
                return;
            }
        }
        // Deferred: orderOut synchronously fires the resign notification, so the dismiss belongs on
        // the next turn, keyed on the showing it was queued for.
        super::set_pending_outside_click(super::showing_generation());
        let _: () = msg_send![
            observer(),
            performSelector: sel!(clipboardOutsideClick:),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

/// The deferred half: re-check that the world has not changed under the queued click.
pub(super) extern "C" fn clipboard_outside_click(_self: *mut c_void, _cmd: Sel) {
    callback_guard::void("clipboard_outside_click", || {
        let Some(generation) = super::take_pending_outside_click() else {
            return;
        };
        // A stale click belongs to a showing that is gone; it must never close the one on screen.
        if !super::showing_is_current(generation) {
            return;
        }
        if super::is_pinned() || !super::picker_is_visible() {
            return;
        }
        hide_picker();
    });
}

/// The screen point of a mouse-down event, or None when it cannot be read.
unsafe fn event_screen_location(event: *mut AnyObject) -> Option<NSPoint> {
    if event.is_null() {
        return None;
    }
    let window: *mut AnyObject = msg_send![event, window];
    let point: NSPoint = msg_send![event, locationInWindow];
    if window.is_null() {
        // Another application's event: AppKit reports locationInWindow in screen coordinates for
        // those, and there is no window to convert through.
        return Some(point);
    }
    Some(msg_send![window, convertPointToScreen: point])
}

/// Whether a screen point lies in the picker's or an open detail panel's VISIBLE rect - never the
/// padded window rect, which is transparent and must not count as "inside".
unsafe fn panel_group_contains(point: NSPoint) -> bool {
    // Only a currently OPEN detail counts. Its window is cached after a close, so its old rect would
    // otherwise keep swallowing outside clicks at a position where nothing is drawn, and the panel
    // could no longer be dismissed there.
    let detail = if super::detail_visible() {
        *super::DETAIL_WINDOW.lock().unwrap()
    } else {
        None
    };
    for window in [*super::PICKER_WINDOW.lock().unwrap(), detail]
        .into_iter()
        .flatten()
    {
        let frame = crate::glass::panel_frame_of(window.0);
        if point.x >= frame.origin.x
            && point.x <= frame.origin.x + frame.size.width
            && point.y >= frame.origin.y
            && point.y <= frame.origin.y + frame.size.height
        {
            return true;
        }
    }
    false
}

/// Picker resign-key notification callback (main thread): auto-hide on outside clicks, etc.
///
/// This stays alongside the click monitor; it is not replaced by it. The monitor handles the case
/// no notification can reach (an unpinned panel that is already non-key); this handles losing key
/// while pinned, which is exactly the event the pin exists to ignore.
extern "C" fn window_did_resign_key(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    callback_guard::void("window_did_resign_key", || {
        if super::is_pinned() {
            // A pinned panel can remain open while the user changes apps. Capture the new external app
            // as focus leaves the panel so a later row click targets the app the user switched to.
            super::paste_handback::capture_external_target();
            unsafe { super::set_picker_glass_inactive(true) };
            return;
        }
        hide_picker();
    });
}

/// A pinned picker remains visible after resigning key; remove its passive-look compensation when
/// it becomes key again so its active material is exactly the configured Liquid Glass.
extern "C" fn window_did_become_key(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    callback_guard::void("window_did_become_key", || {
        unsafe { super::set_picker_glass_inactive(false) };
    });
}

/// The NSEventType values that mean a mouse button went down: left, right, other.
pub(super) fn is_mouse_press_type(event_type: u64) -> bool {
    matches!(event_type, 1 | 3 | 25)
}

/// The NSEventType values that mean a mouse button came up: left, right, other.
pub(super) fn is_mouse_release_type(event_type: u64) -> bool {
    matches!(event_type, 2 | 4 | 26)
}

/// Whether a press at `point` (window base coordinates) lands on the search field, where the click
/// belongs to the field itself (caret placement, the drawn clear ×).
unsafe fn search_field_contains_window_point(point: NSPoint) -> bool {
    let field = match *SEARCH_FIELD.lock().unwrap() {
        Some(field) => field.0,
        None => return false,
    };
    if field.is_null() {
        return false;
    }
    let local: NSPoint =
        msg_send![field, convertPoint: point, fromView: std::ptr::null::<AnyObject>()];
    let bounds: NSRect = msg_send![field, bounds];
    rect_contains_point(bounds, local)
}

/// End the search field's editing session and give the keyboard back to the list.
///
/// AppKit resigns a text control's field editor only when another view takes first responder, and
/// this panel is built from surfaces that never do it on a press: the rows are buttons, the blank
/// regions belong to the drag handlers, the header and footer controls are buttons. Without this
/// the field editor stayed in place after a click elsewhere -- the caret and the focused ring
/// stayed on, and every later keystroke kept filtering the list instead of driving it.
///
/// The handoff is the same one ↓ performs: `makeFirstResponder:` ends the edit (AppKit posts
/// NSControlTextDidEndEditingNotification, which restores the unfocused ring) and leaves the list
/// container owning the keyboard commands. Returns whether an editing session was ended.
pub(super) fn end_search_editing() -> bool {
    unsafe {
        let field = match *SEARCH_FIELD.lock().unwrap() {
            Some(field) => field.0,
            None => return false,
        };
        if field.is_null() {
            return false;
        }
        // `currentEditor` is non-nil exactly while this field's field editor is active.
        let editor: *mut AnyObject = msg_send![field, currentEditor];
        if editor.is_null() {
            return false;
        }
        let window = match *PICKER_WINDOW.lock().unwrap() {
            Some(window) => window.0,
            None => return false,
        };
        let Some(container) = picker_container_ptr() else {
            return false;
        };
        let accepted: bool = msg_send![window, makeFirstResponder: container];
        let responder: *mut AnyObject = msg_send![window, firstResponder];
        let ended = accepted && responder == container;
        if ended {
            log_debug!("[clip] search editing ended by a press outside the field");
        }
        ended
    }
}

/// A press in the picker window that misses the search field ends its editing session; a press on
/// the field keeps it (the field owns caret placement and its drawn clear ×). See
/// [`end_search_editing`] for why the panel has to end the edit itself.
pub(super) fn end_search_editing_for_press(point: NSPoint) -> bool {
    if unsafe { search_field_contains_window_point(point) } {
        return false;
    }
    end_search_editing()
}

pub(super) extern "C" fn clipboard_window_send_event(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut AnyObject,
) {
    callback_guard::void("clipboard_window_send_event", || {
        unsafe {
            let window = _self as *mut AnyObject;
            let etype: u64 = msg_send![event, type];
            // A press that misses the search field ends the field's editing session: none of the
            // panel's other surfaces becomes first responder on a press (the rows are buttons, the
            // blank regions start a drag, the header/footer controls are buttons), so AppKit would
            // leave the field editor in place -- the caret and the focused ring would stay, and the
            // next keystroke would keep filtering instead of driving the list.
            if is_mouse_press_type(etype) {
                let point: NSPoint = msg_send![event, locationInWindow];
                end_search_editing_for_press(point);
            }
            // The options popover dismisses on any click outside its frame (the event still goes
            // through, so the clicked control behaves normally). A DRAG of the panel is not an
            // outside click even though its press lands outside the menu's frame: the blank region
            // is how the panel is moved, and closing the menu under the user's hand would make them
            // reopen it after every move. A press cannot tell a drag from a click yet, so the
            // decision is made on the release: by then the drag has either passed its threshold
            // (`moved`) or it was a plain click and the menu closes as before.
            if crate::clipboard::filter_popover_open() && is_mouse_press_type(etype) {
                let point: NSPoint = msg_send![event, locationInWindow];
                let inside = if let Some(popover) = *FILTER_POPOVER.lock().unwrap() {
                    let frame: NSRect = msg_send![popover.0, frame];
                    let superview: *mut AnyObject = msg_send![popover.0, superview];
                    // The frame is in the (flipped) parent's coords; convert to window space.
                    let in_window: NSRect = msg_send![
                        superview,
                        convertRect: frame,
                        toView: std::ptr::null::<AnyObject>()
                    ];
                    crate::clipboard::rect_contains_point(in_window, point)
                } else {
                    false
                };
                // The options orb sits outside the popover. Keep its click for the orb's own
                // toggle; dismissing here first would make the toggle reopen the menu.
                let on_options_orb = if let Some(orb) = *PICKER_OPTIONS_ORB.lock().unwrap() {
                    let frame: NSRect = msg_send![orb.0, frame];
                    let superview: *mut AnyObject = msg_send![orb.0, superview];
                    let in_window: NSRect = msg_send![
                        superview,
                        convertRect: frame,
                        toView: std::ptr::null::<AnyObject>()
                    ];
                    crate::clipboard::rect_contains_point(in_window, point)
                } else {
                    false
                };
                // Not dismissed here even though the press is outside: a press that turns into a
                // drag of the panel must leave the menu alone, and only the release can tell the
                // two apart. The release resolves it (`drag::finish_popover_press`).
                super::drag::note_popover_outside_press(!inside && !on_options_orb);
            }
            // The release half of the popover's outside-click rule. The drag's own mouseUp is
            // delivered inside `super sendEvent:` below, so this runs FIRST: the grab is still
            // recorded here, which is exactly what tells a drag (keep the menu) from a click
            // (close it). Every release resolves it, so a press that never became a drag cannot
            // leave a dismissal pending into the next gesture.
            if is_mouse_release_type(etype) {
                super::drag::finish_popover_press();
            }
            type SendEvent = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut AnyObject);
            // The class that defined sendEvent:, recorded when the picker window was registered.
            let defined = *crate::clipboard::picker_window_class();
            let mut sup = ObjcSuper::for_defined_class(window as *mut c_void, defined);
            let send_event: SendEvent = std::mem::transmute(objc_msgSendSuper as *const ());
            send_event(&mut sup, sel!(sendEvent:), event);
        }
    });
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
            NSPoint::new(knob_pos, clip_bounds.size.height + 3.0),
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
    let parent: *mut AnyObject = msg_send![indicator, superview];
    let frame = if parent != scroll && !parent.is_null() {
        msg_send![scroll, convertRect: frame, toView: parent]
    } else {
        frame
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
    callback_guard::bool("scroll_indicator_accepts_first_mouse", true, || true)
}

/// Convert thumb movement into document offset. The system scroller is disabled, so NSView
/// does not provide this behavior automatically.
extern "C" fn scroll_indicator_mouse_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    callback_guard::void("scroll_indicator_mouse_down", || unsafe {
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
    });
}

extern "C" fn scroll_indicator_mouse_dragged(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    callback_guard::void("scroll_indicator_mouse_dragged", || unsafe {
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
    });
}

extern "C" fn scroll_indicator_mouse_up(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    callback_guard::void("scroll_indicator_mouse_up", || {
        *SCROLL_DRAG.lock().unwrap() = None;
    });
}

/// Clip-view bounds-change notification callback (scrolling) -> update the indicator; with
/// the detail open, move it along so it keeps following the selected row (otherwise a
/// scroll would leave the detail misaligned with its row).
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
extern "C" fn scroll_indicator_bounds_changed(_self: *mut c_void, _cmd: Sel, _note: *mut c_void) {
    callback_guard::void("scroll_indicator_bounds_changed", || unsafe {
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
    callback_guard::void("detail_scroll_indicator_bounds_changed", || {
        // update_scroll_indicator_for reads DETAIL_SCROLL_VIEW itself and returns silently
        // when the view is gone.
        unsafe {
            update_scroll_indicator_for(ScrollTarget::Detail);
            update_scroll_indicator_for(ScrollTarget::DetailHorizontal);
        }
    });
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
    callback_guard::void("expire_clipboard_undo", || {
        expire_deleted_clipboard_entry();
    });
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

pub(super) extern "C" fn clear_clipboard_history(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("clear_clipboard_history", || {
        clear_clipboard_history_scope(false);
    });
}

pub(super) extern "C" fn clear_clipboard_all(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("clear_clipboard_all", || {
        clear_clipboard_history_scope(true);
    });
}

/// The panel-level pin: toggle the whole-panel state and reflect it on the button.
///
/// Pinning captures the panel's CURRENT height and freezes it there (design decision E), which is
/// why the height comes from the live panel rect rather than from the geometry constants. The pin
/// does not move, resize or re-front the panel, and it never touches the entries.
pub(super) extern "C" fn toggle_panel_pin(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("toggle_panel_pin", || unsafe {
        if super::is_pinned() {
            // Decision C: unpinning only flips the state. The panel stays visible and the next
            // outside click dismisses it. The window properties are NOT restored here: doing so on a
            // panel that is still meant to be on screen can move it off the current Space or make it
            // vanish, which is exactly the cross-Space case the design must survive. The restore is
            // deferred to the actual hide (see `apply_pin_window_properties_on_hide`).
            super::unpin();
        } else {
            let height = match *super::PICKER_WINDOW.lock().unwrap() {
                Some(window) => crate::glass::panel_frame_of(window.0).size.height,
                None => return,
            };
            if !super::pin(height) {
                return;
            }
            // The click monitor is what makes unpinning meaningful (the next outside click closes
            // the panel), so a pin that cannot arm it is refused outright rather than left in a state
            // it cannot honour.
            if !sync_outside_click_monitor() {
                refuse_pin_unavailable();
                return;
            }
            apply_pin_window_properties();
        }
        refresh_pin_button();
        // The menu item mirrors the pin, so the two surfaces cannot drift apart.
        crate::menu::refresh_clipboard_pin_item();
    });
}

/// Refuse a pin whose click monitor could not be armed. The monitor is what makes unpinning
/// meaningful -- the next outside click closes the panel -- so a pin that cannot arm it is undone
/// outright rather than left in a state the panel cannot honour. Shared by the corner button and
/// the menu-bar action so the two surfaces cannot drift apart in what they undo or say.
pub(super) unsafe fn refuse_pin_unavailable() {
    super::forget_pin();
    refresh_pin_button();
    crate::menu::refresh_clipboard_pin_item();
    show_toast(&t("clipboard.toast_pin_unavailable"));
}

/// Reflect the pin on the corner button.
pub(super) unsafe fn refresh_pin_button() {
    if let Some(button) = *super::PICKER_PIN_BUTTON.lock().unwrap() {
        set_pin_button_style(button.0, super::is_pinned(), false);
    }
}

/// Remove the click monitors unconditionally. Used by the hide path, where `picker_is_visible` can
/// still answer true because the orderOut happens later and outside the state lock.
pub(super) unsafe fn remove_outside_click_monitor_now() {
    remove_outside_click_monitor();
}

/// Give a newly created detail window the properties the PICKER is actually carrying right now.
///
/// Deliberately a copy rather than a recomputation from the pin: after unpinning, the still-visible
/// picker keeps the pinned properties until the panel genuinely hides (that is the transition
/// decision C requires), so "recompute from is_pinned()" would restore them early - exactly the
/// defect that broke a cross-Space unpin followed by opening the detail.
pub(super) unsafe fn inherit_window_properties_from_picker(detail: *mut AnyObject) {
    let Some(picker) = (*super::PICKER_WINDOW.lock().unwrap()).map(|w| w.0) else {
        return;
    };
    if detail.is_null() {
        return;
    }
    let hides: bool = msg_send![picker, hidesOnDeactivate];
    let behavior: usize = msg_send![picker, collectionBehavior];
    let _: () = msg_send![detail, setHidesOnDeactivate: hides];
    let _: () = msg_send![detail, setCollectionBehavior: behavior];
}

/// Restore the default window properties for a panel that is actually being hidden.
///
/// Separate from `apply_pin_window_properties` so the two moments cannot be confused: this one is
/// only correct once the panel is genuinely going away, and it deliberately does NOT clear the pin -
/// a hidden pinned panel is still pinned (the menu item brings it back).
pub(super) unsafe fn apply_pin_window_properties_for_hidden_panel() {
    for window in [
        *super::PICKER_WINDOW.lock().unwrap(),
        *super::DETAIL_WINDOW.lock().unwrap(),
    ]
    .into_iter()
    .flatten()
    {
        let _: () = msg_send![window.0, setHidesOnDeactivate: true];
        let _: () = msg_send![window.0, setCollectionBehavior: 0usize];
    }
}

/// Apply the visible panel's pin properties.
///
/// `hidesOnDeactivate` stays disabled while the panel is visible, pinned or not. AppKit can hide a
/// nonactivating panel as its owner deactivates without sending our resign-key callback; that
/// bypasses the unpinned outside-click/resign handling and leaves the visibility state out of sync.
/// Those explicit handlers own dismissal while visible. The pinned collection behavior is what
/// lets a pinned panel follow the active Space (decision F).
///
/// Callers: the pin toggle, menu pin action, and show path. Defaults are restored only after the
/// panel is actually hidden (see `apply_pin_window_properties_for_hidden_panel`).
pub(super) unsafe fn apply_pin_window_properties() {
    let pinned = super::is_pinned();
    let windows = [
        *super::PICKER_WINDOW.lock().unwrap(),
        *super::DETAIL_WINDOW.lock().unwrap(),
    ];
    for window in windows.into_iter().flatten() {
        let _: () = msg_send![window.0, setHidesOnDeactivate: false];
        if pinned {
            let _: () = msg_send![window.0, setCollectionBehavior: PINNED_COLLECTION_BEHAVIOR];
        } else {
            let _: () = msg_send![window.0, setCollectionBehavior: 0usize];
        }
    }
}
/// The header's corner close button: the same dismissal as Esc with no query / an outside click.
///
/// This is the ONE path that clears the panel pin (design decision A): the x means the user is
/// done with the panel, while every other dismissal only hides it.
extern "C" fn picker_close(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("picker_close", || {
        super::forget_pin();
        unsafe {
            refresh_pin_button();
            apply_pin_window_properties();
            sync_outside_click_monitor();
        }
        crate::menu::refresh_clipboard_pin_item();
        hide_picker();
    });
}

/// The header's settings button: leave the panel and land on the clipboard page of the settings
/// window. The panel goes first -- it is a floating overlay and the settings window is a real
/// window, so keeping both up would leave a stale panel behind the window the user just asked for.
/// `pub(super)` so the smoke runner can run the very action the button targets.
/// The gear click's full in-app response, in milliseconds: from the timestamp the click's event carries, past
/// the settings window coming up, to the panel being down (see `e2e_settings_click_ms`).
static SETTINGS_CLICK_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The part of `SETTINGS_CLICK_MS` spent before this handler ran (event delivery), so the number above can
/// be read apart from the wait for the main thread.
static SETTINGS_CLICK_DELIVERY_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// The click path's measurement, published in `--e2e-state` as `settings_click_ms`. It is wider than
/// `settings_open_ms` (the window build/select/present alone) and is what a "how long until the settings
/// window is up" bound should use.
pub(crate) fn e2e_settings_click_ms() -> u64 {
    SETTINGS_CLICK_MS.load(Ordering::SeqCst)
}

/// The event-delivery part of `e2e_settings_click_ms`, published as `settings_click_delivery_ms`: a bound on
/// the whole path is only meaningful if this is measured too, not assumed to be zero.
pub(crate) fn e2e_settings_click_delivery_ms() -> u64 {
    SETTINGS_CLICK_DELIVERY_MS.load(Ordering::SeqCst)
}

pub(super) extern "C" fn open_clipboard_settings(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("open_clipboard_settings", || {
        // Bring the settings window up FIRST, then take the panel down: the other order left a moment with
        // neither surface on screen (the panel gone, the window not yet composed), which read as a stall on
        // top of the window's own build cost.
        let started = std::time::Instant::now();
        // How long the click waited before this handler ran. `NSEvent.timestamp` and
        // `NSProcessInfo.systemUptime` share the clock (seconds since boot), so the difference is the part of
        // the response the handler itself cannot see -- measured rather than assumed to be zero.
        let delivery_ms = unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let event: *mut AnyObject = msg_send![nsapp, currentEvent];
            if event.is_null() {
                0.0
            } else {
                let timestamp: f64 = msg_send![event, timestamp];
                let info: *mut AnyObject = msg_send![class!(NSProcessInfo), processInfo];
                let uptime: f64 = msg_send![info, systemUptime];
                ((uptime - timestamp) * 1000.0).max(0.0)
            }
        };
        crate::settings::show_clipboard_page();
        let shown = started.elapsed();
        // A pinned panel stays up and ordered front (decision R1): the gear is a navigation action, not
        // a dismissal, so it must not take the pinned panel away with it.
        if !super::is_pinned() {
            hide_picker();
        }
        let total = started.elapsed();
        // Debug-level, click-path timing, and the number the A2 latency scenario bounds: from the click the
        // event carries to the moment the panel is down and the window is up. Only the WindowServer's own
        // compositing after this point is outside what the app can measure.
        SETTINGS_CLICK_DELIVERY_MS.store(delivery_ms.round() as u64, Ordering::SeqCst);
        SETTINGS_CLICK_MS.store(
            (total.as_millis() + delivery_ms.round() as u128).min(u64::MAX as u128) as u64,
            Ordering::SeqCst,
        );
        log_debug!(
            "[settings-timing] gear_click delivery={}ms show={}ms hide={}ms total={}ms",
            delivery_ms.round() as u64,
            shown.as_millis(),
            total.as_millis().saturating_sub(shown.as_millis()),
            total.as_millis() + delivery_ms.round() as u128
        );
    });
}

/// Clear the visible history scope (see `remove_history_scope`) and release the cache files of the
/// entries that went away, then rewrite the file and refresh the list. The plain clear keeps the
/// pinned entries; only the explicit "clear all" button takes them.
fn clear_clipboard_history_scope(clear_all: bool) {
    clear_clipboard_scope(clear_all, None);
}

/// Clear one day group (the group header's "clear" button). The group is an ADDITIONAL narrowing on
/// top of the visible scope, so this goes through the same deletion flow as the header's clear
/// instead of filtering the history by date on its own: the image cache, the revision, the detail
/// pane and the file all stay in step, and hidden entries are never taken with it.
fn clear_clipboard_day_group(group: DayGroup) {
    clear_clipboard_scope(false, Some(group));
}

fn clear_clipboard_scope(clear_all: bool, group: Option<DayGroup>) {
    discard_deleted_clipboard_entry();
    cancel_clipboard_undo_timer();
    DELETED_CLIPBOARD_GENERATION.fetch_add(1, Ordering::SeqCst);
    // The scope is what the user is looking at: the active filter narrowed by the search query. The
    // query stays in the field -- the list then shows whatever is left of it, which is the honest
    // result of clearing the visible set.
    let filter = *CLIP_FILTER.lock().unwrap();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let mut history = CLIP_HISTORY.lock().unwrap();
    let removed_entries = match group {
        Some(group) => remove_day_group_scope(&mut history, clear_all, filter, &query, group),
        None => remove_history_scope(&mut history, clear_all, filter, &query),
    };
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
        "Clipboard history cleared by user (clear_all={}, group={:?}, filter={:?}, query={}, kept_entries={})",
        clear_all,
        group,
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
    callback_guard::void("search_field_changed", || {
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
    });
}

/// NSSearchField's Esc (cancelOperation:): a query gets cleared and the full list restored
/// (scheme A, level one); with no query the picker closes (level two).
pub(super) extern "C" fn search_field_cancel(_self: *mut c_void, _cmd: Sel) {
    callback_guard::void("search_field_cancel", || {
        // A live options menu owns the first Escape press regardless of which field has focus.
        if crate::clipboard::filter_popover_open() {
            super::detail::dismiss_filter_popover();
            return;
        }
        // Same rule as the list-focus Esc: a pinned panel ignores the key before it can clear a query.
        if super::is_pinned() {
            return;
        }
        let has_query = with_clipboard_ui(|ui| !ui.search_query.is_empty());
        if has_query {
            clear_search();
            // Focus stays in the search field: keep "no selection" (no highlight returns).
            unsafe { rebuild_rows() };
        } else {
            hide_picker();
        }
    });
}

/// The search field's right clear button: do not route through responder-chain cancelOperation:
/// (which the field editor may intercept); clear the field and filter directly. Unlike Esc,
/// clicking an already-empty field never closes the picker.
pub(super) extern "C" fn search_clear_button(_self: *mut c_void, _cmd: Sel, _sender: *mut c_void) {
    callback_guard::void("search_clear_button", || {
        clear_search();
        unsafe { rebuild_rows() };
    });
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
    callback_guard::void("search_field_mouse_moved", || unsafe {
        update_search_clear_hover(_self as *mut AnyObject, event)
    });
}

pub(super) extern "C" fn search_field_mouse_entered(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    callback_guard::void("search_field_mouse_entered", || unsafe {
        update_search_clear_hover(_self as *mut AnyObject, event)
    });
}

pub(super) extern "C" fn search_field_mouse_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    callback_guard::void("search_field_mouse_exited", || {
        if SEARCH_CLEAR_HOVERED.swap(false, Ordering::SeqCst) {
            unsafe {
                let field = _self as *mut AnyObject;
                let _: () = msg_send![field, setNeedsDisplay: true];
            }
        }
    });
}

pub(super) extern "C" fn search_field_mouse_down(
    _self: *mut c_void,
    _cmd: Sel,
    event: *mut c_void,
) {
    callback_guard::void("search_field_mouse_down", || {
        unsafe {
            let field = _self as *mut AnyObject;
            if search_clear_contains_event(field, event) {
                search_clear_button(_self, sel!(clearSearch:), event);
                return;
            }
            type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void) -> ();
            // The class that defined mouseDown: here (registered in detail.rs).
            let defined = *crate::clipboard::search_field_class();
            let mut sup = ObjcSuper::for_defined_class(_self, defined);
            let f: F = std::mem::transmute(objc_msgSendSuper as *const ());
            f(&mut sup, sel!(mouseDown:), event);
        }
    });
}

/// The search field's fill/ring helper (raw FFI for the layer background). Focus strengthens the
/// fill and the ring together, as the v2 mock's `.search-field:focus-within` does; the palette
/// supplies both values in dark mode.
pub(super) unsafe fn style_search_field(field: *mut AnyObject, _focused: bool) {
    let layer: *mut AnyObject = msg_send![field, layer];
    if !layer.is_null() {
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(0));
        let _: () = msg_send![layer, setBorderWidth: 0.0f64];
    }
    if let Some(surface) = *SEARCH_FIELD_SURFACE.lock().unwrap() {
        let surface_layer: *mut AnyObject = msg_send![surface.0, layer];
        if !surface_layer.is_null() {
            crate::ffi::layer_set_background(
                surface_layer,
                crate::ffi::hex_to_cg_color(picker_field_fill(_focused)),
            );
            crate::ffi::layer_set_border(
                surface_layer,
                crate::ffi::hex_to_cg_color(picker_field_ring(_focused)),
            );
        }
    }
    let _: () = msg_send![field, setNeedsDisplay: true];
}

/// Editing begins: keep the default 4.5% frosted fill and use only a 10% inner ring for focus,
/// avoiding a disruptive white transition while typing.
extern "C" fn search_focus_began(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    callback_guard::void("search_focus_began", || {
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
    });
}

/// Editing ends: restore the default inner ring.
extern "C" fn search_focus_ended(_self: *mut c_void, _cmd: Sel, note: *mut c_void) {
    callback_guard::void("search_focus_ended", || {
        unsafe {
            let field: *mut AnyObject = msg_send![note as *mut AnyObject, object];
            if !field.is_null() {
                style_search_field(field, false);
                // Same as search_focus_began: restoring the placeholder on blur needs an
                // immediate redraw.
                let _: () = msg_send![field, setNeedsDisplay: true];
            }
        }
    });
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
    callback_guard::bool("search_field_do_command", true, || {
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
    })
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

/// The options orb: toggles the filter/options popover (the flag flips here so `--e2e-state`
/// reports it even while the view is being created).
pub(super) extern "C" fn toggle_filter_popover(
    _self: *mut c_void,
    _cmd: Sel,
    _sender: *mut c_void,
) {
    callback_guard::void("toggle_filter_popover", || {
        crate::clipboard::toggle_filter_popover();
    });
}

/// A filter row in the options popover: its tag indexes `CLIP_FILTER_ROWS`. Choosing a row
/// applies the filter and dismisses the popover.
pub(super) extern "C" fn popover_filter_selected(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    callback_guard::void("popover_filter_selected", || {
        let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        let Some(&f) = CLIP_FILTER_ROWS.get(tag as usize) else {
            return;
        };
        super::detail::dismiss_filter_popover();
        apply_clip_filter(f);
    });
}

/// A group label's "清除" button: drops that day group's *visible* entries through the ordinary
/// clear flow (pinned entries stay, the image cache and the detail pane follow).
pub(super) extern "C" fn clear_day_group(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    callback_guard::void("clear_day_group", || {
        let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
        let Some(group) = model::day_group_from_tag(tag) else {
            return;
        };
        clear_clipboard_day_group(group);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// APPLIED_NOTICE is process-global, so every test that writes it serializes on this lock.
    static APPLIED_NOTICE_LOCK: Mutex<()> = Mutex::new(());

    fn lock_and_reset_applied_notice() -> std::sync::MutexGuard<'static, ()> {
        let guard = APPLIED_NOTICE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *APPLIED_NOTICE.lock().unwrap() = AppliedNotice::new();
        guard
    }

    #[test]
    fn unavailable_states_offer_the_keychain_grant() {
        for reason in [
            KeyUnavailable::Simulated,
            KeyUnavailable::Missing,
            KeyUnavailable::AccessDenied,
            KeyUnavailable::Other,
        ] {
            assert_eq!(
                storage_action_key(&StorageState::Unavailable(reason)),
                StorageAction::GrantKeychain
            );
        }
    }

    #[test]
    fn clearable_blocked_states_offer_clear_and_restart() {
        for reason in [
            BlockedReason::Damaged,
            BlockedReason::ForeignKey,
            BlockedReason::UnsupportedVersion,
            BlockedReason::MigrationIncomplete,
        ] {
            assert_eq!(
                storage_action_key(&StorageState::Blocked(reason)),
                StorageAction::ClearAndRestart
            );
        }
    }

    #[test]
    fn a_pending_purge_and_the_healthy_states_offer_no_action() {
        assert_eq!(
            storage_action_key(&StorageState::Blocked(BlockedReason::PurgePending)),
            StorageAction::None
        );
        assert_eq!(
            storage_action_key(&StorageState::Uninitialized),
            StorageAction::None
        );
        assert_eq!(
            storage_action_key(&StorageState::Ready),
            StorageAction::None
        );
    }

    #[test]
    fn a_held_key_reports_granted_for_every_state() {
        for state in [
            StorageState::Uninitialized,
            StorageState::Ready,
            StorageState::Unavailable(KeyUnavailable::Missing),
            StorageState::Blocked(BlockedReason::PurgePending),
            StorageState::Blocked(BlockedReason::Damaged),
        ] {
            assert_eq!(
                keychain_row_status_key(&state, true),
                ("settings.keychain_status_granted", false),
                "state {state:?}"
            );
        }
    }

    #[test]
    fn a_missing_key_is_the_only_status_that_offers_the_action() {
        assert_eq!(
            keychain_row_status_key(
                &StorageState::Unavailable(KeyUnavailable::AccessDenied),
                false
            ),
            ("settings.keychain_status_missing", true)
        );
    }

    #[test]
    fn a_pending_purge_reports_cleanup_without_the_action() {
        assert_eq!(
            keychain_row_status_key(&StorageState::Blocked(BlockedReason::PurgePending), false),
            ("settings.keychain_status_cleanup", false)
        );
    }

    #[test]
    fn other_blocked_states_report_unreadable_without_the_action() {
        for reason in [
            BlockedReason::Damaged,
            BlockedReason::ForeignKey,
            BlockedReason::UnsupportedVersion,
            BlockedReason::MigrationIncomplete,
        ] {
            assert_eq!(
                keychain_row_status_key(&StorageState::Blocked(reason), false),
                ("settings.keychain_status_unreadable", false)
            );
        }
    }

    #[test]
    fn uninitialized_and_ready_report_not_read() {
        assert_eq!(
            keychain_row_status_key(&StorageState::Uninitialized, false),
            ("settings.keychain_status_not_read", false)
        );
        assert_eq!(
            keychain_row_status_key(&StorageState::Ready, false),
            ("settings.keychain_status_not_read", false)
        );
    }

    #[test]
    fn applied_action_labels_are_stable() {
        assert_eq!(AppliedAction::Absent.label(), "absent");
        assert_eq!(AppliedAction::Hidden.label(), "hidden");
        assert_eq!(AppliedAction::Grant.label(), "grant");
        assert_eq!(AppliedAction::Clear.label(), "clear");
    }

    #[test]
    fn applied_notice_labels_start_unset() {
        let _guard = lock_and_reset_applied_notice();
        assert_eq!(
            applied_notice_labels(),
            ("unset", "unset", "unset", "unset")
        );
    }

    #[test]
    fn recording_a_surface_updates_only_its_own_slot() {
        let _guard = lock_and_reset_applied_notice();
        record_applied_action(NoticeSurface::SettingsBanner, AppliedAction::Grant);
        assert_eq!(
            applied_notice_labels(),
            ("grant", "unset", "unset", "unset")
        );
        record_applied_action(NoticeSurface::PickerEmptyState, AppliedAction::Clear);
        assert_eq!(
            applied_notice_labels(),
            ("grant", "clear", "unset", "unset")
        );
        record_applied_action(NoticeSurface::AboutRow, AppliedAction::Hidden);
        assert_eq!(
            applied_notice_labels(),
            ("grant", "clear", "hidden", "unset")
        );
    }

    #[test]
    fn the_about_status_is_recorded_separately_from_the_about_action() {
        let _guard = lock_and_reset_applied_notice();
        record_applied_about_status("unreadable");
        assert_eq!(
            applied_notice_labels(),
            ("unset", "unset", "unset", "unreadable")
        );
        record_applied_action(NoticeSurface::AboutRow, AppliedAction::Clear);
        assert_eq!(
            applied_notice_labels(),
            ("unset", "unset", "clear", "unreadable")
        );
    }

    #[test]
    fn unset_is_not_the_absent_label() {
        let _guard = lock_and_reset_applied_notice();
        assert_eq!(applied_notice_labels().0, "unset");
        record_applied_action(NoticeSurface::SettingsBanner, AppliedAction::Absent);
        assert_eq!(applied_notice_labels().0, "absent");
    }

    #[test]
    fn unavailable_copy_key_classifies_every_storage_state() {
        for reason in [
            KeyUnavailable::Simulated,
            KeyUnavailable::Missing,
            KeyUnavailable::AccessDenied,
            KeyUnavailable::Other,
        ] {
            assert_eq!(
                unavailable_copy_key(&StorageState::Unavailable(reason)),
                Some("clipboard.unavailable_key")
            );
        }
        assert_eq!(
            unavailable_copy_key(&StorageState::Blocked(BlockedReason::PurgePending)),
            Some("clipboard.unavailable_purge")
        );
        for reason in [BlockedReason::MigrationIncomplete, BlockedReason::Damaged] {
            assert_eq!(
                unavailable_copy_key(&StorageState::Blocked(reason)),
                Some("clipboard.unavailable_history_retry")
            );
        }
        for reason in [BlockedReason::ForeignKey, BlockedReason::UnsupportedVersion] {
            assert_eq!(
                unavailable_copy_key(&StorageState::Blocked(reason)),
                Some("clipboard.unavailable_history")
            );
        }
        assert_eq!(unavailable_copy_key(&StorageState::Uninitialized), None);
        assert_eq!(unavailable_copy_key(&StorageState::Ready), None);
    }
}
