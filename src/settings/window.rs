//! Settings window classes and lifecycle (show/hide/sidebar switching) plus content construction.

use super::*;
use crate::clipboard::{
    record_applied_about_status, record_applied_action, AppliedAction, NoticeSurface,
};

/// The sidebar buttons, indexed by sidebar order. The slots are stored in that order, so this is
/// the array itself -- there is no second per-page mapping left to drift from it.
pub(super) fn sidebar_buttons(ui: &SettingsUi) -> [*mut AnyObject; SETTINGS_PAGE_COUNT] {
    ui.sidebar_buttons
}

/// The page scroll views, indexed by sidebar order. Same rule as [`sidebar_buttons`].
pub(super) fn page_scroll_views(ui: &SettingsUi) -> [*mut AnyObject; SETTINGS_PAGE_COUNT] {
    ui.page_views
}

/// Which warning a page's top strip carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageWarning {
    /// Nothing to report: the strip stays hidden.
    None,
    /// A missing permission, with the "Open Privacy & Security" button.
    Permission,
    /// The clipboard storage failure (no key, or nothing readable).
    Storage,
}

/// Pick the warning for `page` from the two facts that can produce one.
///
/// General reports a missing permission first — without it the shortcut does not work at all — and
/// otherwise the clipboard storage failure, which is an app-level fact worth stating where settings
/// opens. The Clipboard page reports the storage failure, which is the page's own subject. Every
/// other page has nothing to say, so the strip is hidden there.
pub(super) fn warning_for_page(
    page: usize,
    has_required_permissions: bool,
    storage_unavailable: bool,
) -> PageWarning {
    match page {
        0 if !has_required_permissions => PageWarning::Permission,
        0 if storage_unavailable => PageWarning::Storage,
        SETTINGS_CLIPBOARD_PAGE_INDEX if storage_unavailable => PageWarning::Storage,
        _ => PageWarning::None,
    }
}

/// The warning the top strip carries for the selected page.
fn selected_page_warning() -> PageWarning {
    let is_permission_migration = crate::update_notice::needs_permission_migration_copy();
    let has_required_permissions = has_accessibility_permission()
        && (!is_permission_migration || crate::thumbnail::capture_allowed());
    warning_for_page(
        SIDEBAR_SELECTED.load(Ordering::SeqCst),
        has_required_permissions,
        crate::clipboard::unavailable_copy().is_some(),
    )
}

/// Recompute the top warning banner from live state, and show it while the selected page has
/// something to report.
pub(super) unsafe fn refresh_settings_warning_banner(ui: &SettingsUi) {
    if ui.permission_warning_view.is_null() {
        return;
    }
    let warning = selected_page_warning();
    let copy = match warning {
        PageWarning::None => None,
        PageWarning::Permission => {
            let key = if crate::update_notice::needs_permission_migration_copy() {
                "settings.permission_migration_warning"
            } else {
                "settings.accessibility_warning"
            };
            Some(t(key))
        }
        PageWarning::Storage => crate::clipboard::unavailable_copy(),
    };
    if !ui.permission_warning_label.is_null() {
        widgets::set_field(ui.permission_warning_label, copy.as_deref().unwrap_or(""));
    }
    // The banner's button follows the warning it carries: the permission copy offers System
    // Settings, the storage copy offers keychain access (there is no System Settings pane for the
    // keychain -- it is not a TCC permission -- so the action has to live in the app).
    if !ui.permission_warning_button.is_null() {
        let (title, action, visible) = match warning {
            PageWarning::Permission => (
                t("settings.btn_open_privacy"),
                sel!(handleOpenPrivacy:),
                true,
            ),
            PageWarning::Storage => match crate::clipboard::storage_action() {
                crate::clipboard::StorageAction::GrantKeychain => (
                    t("clipboard.grant_keychain_access"),
                    sel!(retryKeychainAccess:),
                    true,
                ),
                // The blocked states cannot be cleared through the keychain: re-reading the key
                // re-derives the same state. Their way out is the purge transaction, which is what
                // this action runs (see `clipboard::clear_storage_and_restart`).
                crate::clipboard::StorageAction::ClearAndRestart => (
                    t("clipboard.clear_and_restart"),
                    sel!(clearClipboardStorage:),
                    true,
                ),
                crate::clipboard::StorageAction::None => {
                    (String::new(), sel!(handleOpenPrivacy:), false)
                }
            },
            PageWarning::None => (String::new(), sel!(handleOpenPrivacy:), false),
        };
        if visible {
            let ns = make_nsstring(&title);
            let _: () = msg_send![ui.permission_warning_button, setTitle: ns];
            let _: () = msg_send![ui.permission_warning_button, setAction: action];
            CFRelease(ns as *const c_void);
        }
        let _: () = msg_send![ui.permission_warning_button, setHidden: !visible];
        // The palette follows the warning: a destructive storage action must read as destructive
        // (red text — the red fill is reserved for a confirming step), and switching back to the
        // permission warning must restore the neutral role. The same button carries both, so the role
        // is re-applied on every refresh instead of only at construction.
        if warning == PageWarning::Storage && action == sel!(clearClipboardStorage:) {
            SettingsButton::apply_destructive_text(ui.permission_warning_button);
        } else {
            SettingsButton::apply_role(ui.permission_warning_button, SettingsButtonRole::Action);
        }
        // Record what this surface actually applied (for `--e2e-state`): the button exists, so the
        // question is which action it carries. A permission banner (or none) carries no storage
        // action at all — that is "hidden", never "clear".
        let applied = match warning {
            PageWarning::Storage if visible && action == sel!(retryKeychainAccess:) => {
                AppliedAction::Grant
            }
            PageWarning::Storage if visible => AppliedAction::Clear,
            _ => AppliedAction::Hidden,
        };
        record_applied_action(NoticeSurface::SettingsBanner, applied);
    } else {
        record_applied_action(NoticeSurface::SettingsBanner, AppliedAction::Absent);
    }
    // The banner is a sibling of the page scroll views, so the page it covers has to give up that
    // height. Reserving against the selected page and restoring every other page (rather than
    // adjusting whichever page was showing) keeps this idempotent: a page switch cannot leave one
    // page short and another long. The full height comes from the pages' parent, which is live:
    // a window resize must not be undone by the next refresh.
    // The pages' parent spans the whole window, INCLUDING the title-bar band: a page sized to it
    // would run up under the band again (the reason a page could draw through the title). The
    // live page frame is the floored height, so that is what the banner reserves against.
    let page = ui.page_view(SettingsPageId::General);
    let base_h = if page.is_null() {
        ui.page_viewport_h
    } else {
        let frame: NSRect = msg_send![page, frame];
        if frame.size.height > 100.0 {
            frame.size.height
        } else {
            ui.page_viewport_h
        }
    };
    let banner_frame: NSRect = msg_send![ui.permission_warning_view, frame];
    let reserved_h = (base_h - banner_frame.size.height).max(1.0);
    for (index, &scroll) in page_scroll_views(ui).iter().enumerate() {
        if scroll.is_null() {
            continue;
        }
        let want = if copy.is_some() && index == SIDEBAR_SELECTED.load(Ordering::SeqCst) {
            reserved_h
        } else {
            base_h
        };
        let mut frame: NSRect = msg_send![scroll, frame];
        if (frame.size.height - want).abs() > 0.5 {
            frame.size.height = want;
            let _: () = msg_send![scroll, setFrame: frame];
        }
    }
    let _: () = msg_send![ui.permission_warning_view, setHidden: copy.is_none()];
}

/// Switch the active settings page: align the highlight to the selected button, toggle the
/// eight content views' visibility, and bold the selected item's label.
/// An editor that belongs to one page and must not outlive a switch away from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PageEditor {
    /// A key-binding chord recording (and the unconfirmed draft it may have produced).
    BindingRecording,
    /// The Key Bindings add panel.
    BindingPanel,
    /// The Mouse page's mapping edit panel.
    MappingPanel,
}

/// Which editor *classes* have to be wrapped up when the sidebar moves to `idx`.
///
/// Pure and free of UI state, so the ownership rule is assertable without a live window: each editor
/// belongs to one page, and a switch away from that page abandons it. Whether a given editor is
/// actually open is a separate question the caller answers with `close_page_editor`, which is also
/// what keeps a closed panel from being disturbed.
pub(super) fn abandoned_page_editors(idx: usize) -> Vec<PageEditor> {
    let mut editors = Vec::new();
    if idx != SETTINGS_KEY_BINDINGS_PAGE_INDEX {
        editors.push(PageEditor::BindingRecording);
        editors.push(PageEditor::BindingPanel);
    }
    if idx != SETTINGS_MOUSE_PAGE_INDEX {
        editors.push(PageEditor::MappingPanel);
    }
    editors
}

/// Wrap up one editor if it is actually live, and report whether it was.
///
/// Each arm asks its own liveness question: closing an already-closed panel would be harmless, but a
/// *recording* cancel must not run on a page the user is only arriving at, and the mapping panel is
/// not this page's to touch unless it is open.
fn close_page_editor(editor: PageEditor) -> bool {
    match editor {
        PageEditor::BindingRecording => {
            if !mapping::pending_binding_recording() {
                return false;
            }
            mapping::cancel_recording_from_main();
            true
        }
        PageEditor::BindingPanel => {
            if !bindings::panel_is_open() {
                return false;
            }
            bindings::close_binding_panel();
            true
        }
        PageEditor::MappingPanel => {
            if !mapping::mapping_panel_is_open() {
                return false;
            }
            mapping::close_mapping_panel();
            true
        }
    }
}

pub(super) fn select_sidebar(idx: usize) {
    // fall back to the General page if the tag is out of range
    let idx = if idx >= SETTINGS_PAGE_COUNT { 0 } else { idx };
    // Leaving a page abandons its editors: a recording, a confirmed-but-unwritten draft and the
    // floating panel that owns them all belong to the page the user is looking at, so none may
    // outlive a switch to another page. The mouse page's mapping panel is closed on the same rule.
    for editor in abandoned_page_editors(idx) {
        close_page_editor(editor);
    }
    // Dismiss the previous page's disabled hint so the bubble cannot remain across tabs.
    unsafe { tooltip::SettingsTooltip::dismiss() };
    unsafe { widgets::clear_sidebar_hover() };
    SIDEBAR_SELECTED.swap(idx, Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui| {
            let ui = match ui.as_ref() {
                Some(u) => u,
                None => return,
            };
            let buttons = sidebar_buttons(ui);
            let views = page_scroll_views(ui);
            // align the highlight to the selected button's frame
            let frame: NSRect = msg_send![buttons[idx], frame];
            SettingsSidebar::move_highlight(ui.sidebar_highlight, frame);
            // Selected items use an accent-colored bold title; unselected items use the system label color.
            for (i, &b) in buttons.iter().enumerate() {
                let layer: *mut AnyObject = msg_send![b, layer];
                if !layer.is_null() {
                    layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
                }
                set_sidebar_title(b, &t(settings_page_identity(i).title_key()), i == idx);
            }
            // Toggle every page's visibility.
            for (i, &v) in views.iter().enumerate() {
                let _: () = msg_send![v, setHidden: i != idx];
            }
            refresh_settings_warning_banner(ui);
            if idx == SETTINGS_ABOUT_PAGE_INDEX {
                refresh_permission_statuses(ui);
            }
            // A just-shown page needs a layout pass first so the clip bounds are correct;
            // then scroll it to the top. layoutIfNeeded lives on the window, not the scroll view.
            let _: () = msg_send![ui.window, layoutIfNeeded];
            let page = SettingsPage {
                scroll: views[idx],
                document: msg_send![views[idx], documentView],
            };
            page.scroll_to_top();
            page.validate(settings_page_name(idx));
        });
    }
    // Landing on the Mouse page has to fill it: the value load skipped it when this open came in on
    // another page. This runs after the closure above, because the materialization re-borrows the settings
    // slot and a nested borrow would be swallowed by the reentrancy guard.
    crate::settings::materialize_mouse_page_if_visible();
    // With a query active the pages are not what the pane shows: the results stand in for them, so
    // the visibility just applied above has to be corrected (and a rebuild's results re-rendered
    // from the current locale). Runs outside the closure for the same reentrancy reason.
    if sidebar::settings_search_is_active() {
        unsafe { sidebar::refresh_settings_search() };
    }
    // A2 E2E: the selection and the highlight pill are parked, so write a geometry snapshot for
    // scripts/e2e. It must sit outside the with_settings_ui closure -- a nested borrow there is
    // silently swallowed by the reentrancy guard.
    crate::e2e_state::record("settings");
}

/// Refresh the About page's live TCC status labels without reloading user settings.
unsafe fn refresh_permission_statuses(ui: &SettingsUi) {
    if !ui.accessibility_permission_status.is_null()
        || !ui.accessibility_permission_button.is_null()
    {
        refresh_accessibility_permission_action(ui);
    }
    refresh_keychain_access_row(ui);
    if !ui.screen_recording_permission_status.is_null() {
        set_permission_status(
            ui.screen_recording_permission_status,
            crate::thumbnail::capture_allowed(),
        );
    }
}

/// Refresh the About page's keychain-access row.
///
/// The row reports the STORAGE state, not merely "is a key held": a blocked history is not a
/// keychain problem (offering the keychain action there is a dead end), and a session that has not
/// loaded yet has not asked for the key at all — right after a purge, "needs authorization" would
/// accuse the keychain of a state the app itself just created. The action button is offered exactly
/// where the keychain action is the right one (see `StorageAction`).
unsafe fn refresh_keychain_access_row(ui: &SettingsUi) {
    if ui.keychain_access_status.is_null() {
        record_applied_action(NoticeSurface::AboutRow, AppliedAction::Absent);
        return;
    }
    KEYCHAIN_ROW_REFRESHES.fetch_add(1, Ordering::SeqCst);
    let (key, show_action) = crate::clipboard::keychain_row_status();
    let granted = key == "settings.keychain_status_granted";
    let color: *mut AnyObject = if granted {
        msg_send![class!(NSColor), systemGreenColor]
    } else {
        msg_send![class!(NSColor), systemOrangeColor]
    };
    let _: () = msg_send![ui.keychain_access_status, setTextColor: color];
    set_field(ui.keychain_access_status, t(key));
    record_applied_about_status(key.strip_prefix("settings.keychain_status_").unwrap_or(key));
    if !ui.keychain_access_button.is_null() {
        let _: () = msg_send![ui.keychain_access_button, setHidden: !show_action];
        record_applied_action(
            NoticeSurface::AboutRow,
            if show_action {
                AppliedAction::Grant
            } else {
                AppliedAction::Hidden
            },
        );
    } else {
        record_applied_action(NoticeSurface::AboutRow, AppliedAction::Absent);
    }
}

unsafe fn set_permission_status(label: *mut AnyObject, granted: bool) {
    let (key, color): (&str, *mut AnyObject) = if granted {
        (
            "settings.permission_status_granted",
            msg_send![class!(NSColor), systemGreenColor],
        )
    } else {
        (
            "settings.permission_status_missing",
            msg_send![class!(NSColor), systemOrangeColor],
        )
    };
    let _: () = msg_send![label, setTextColor: color];
    set_field(label, t(key));
}

unsafe fn refresh_accessibility_permission_action(ui: &SettingsUi) {
    let granted = has_accessibility_permission();
    let restart_required = granted && crate::restart::restart_required();
    if !ui.accessibility_permission_status.is_null() {
        if restart_required {
            let color: *mut AnyObject = msg_send![class!(NSColor), systemOrangeColor];
            let _: () = msg_send![ui.accessibility_permission_status, setTextColor: color];
            set_field(
                ui.accessibility_permission_status,
                t("settings.permission_status_restart_required"),
            );
        } else {
            set_permission_status(ui.accessibility_permission_status, granted);
        }
    }
    if !ui.accessibility_permission_button.is_null() {
        let (title, action) = if restart_required {
            (
                t("settings.btn_restart_to_restore_shortcuts"),
                sel!(handleRestartForAccessibility:),
            )
        } else {
            (
                t("settings.btn_open_permission_settings"),
                sel!(handleOpenPrivacy:),
            )
        };
        let title = make_nsstring(&title);
        let _: () = msg_send![ui.accessibility_permission_button, setTitle: title];
        let _: () = msg_send![ui.accessibility_permission_button, setAction: action];
        CFRelease(title as *const c_void);
    }
}

/// Run the main run loop for up to `seconds`, so work queued with `performSelectorOnMainThread`
/// (the clipboard load's apply step, the settings refresh) actually runs. Smoke runners drive the app
/// from the main thread and never yield otherwise.
unsafe fn pump_run_loop_for(seconds: f64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    while std::time::Instant::now() < deadline {
        let _ = crate::ffi::CFRunLoopRunInMode(crate::ffi::kCFRunLoopDefaultMode, 0.02, 1u8);
    }
}

/// How many times the About page's keychain row has been repainted. Exposed because "a silent grant
/// repaints the row" is otherwise unobservable: the smoke drives the app from the main thread, so it
/// can read the row but cannot tell which code path wrote it. A counter-example (removing the
/// production refresh) makes the smoke fail, so this cannot pass by chance.
static KEYCHAIN_ROW_REFRESHES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(crate) fn keychain_row_refresh_count() -> usize {
    KEYCHAIN_ROW_REFRESHES.load(Ordering::SeqCst)
}

/// Refresh the visible permission UI when the app regains focus.
pub(crate) fn refresh_permission_ui_if_visible() {
    let selected_page = SIDEBAR_SELECTED.load(Ordering::SeqCst);
    if selected_page != 0 && selected_page != SETTINGS_ABOUT_PAGE_INDEX {
        return;
    }
    with_settings_ui(|ui| {
        if let Some(ui) = ui.as_ref() {
            unsafe {
                let visible: bool = msg_send![ui.window, isVisible];
                if visible {
                    if selected_page == 0 {
                        refresh_settings_warning_banner(ui);
                    } else {
                        refresh_permission_statuses(ui);
                    }
                }
            }
        }
    });
}

/// Center the settings window in the visible area of the screen under the cursor, excluding the
/// menu bar and Dock.
unsafe fn center_settings_window(window: *mut AnyObject) {
    let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    let mut visible_frame: Option<NSRect> = None;

    for i in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
        let frame: NSRect = msg_send![screen, frame];
        if cursor.x >= frame.origin.x
            && cursor.x <= frame.origin.x + frame.size.width
            && cursor.y >= frame.origin.y
            && cursor.y <= frame.origin.y + frame.size.height
        {
            visible_frame = Some(msg_send![screen, visibleFrame]);
            break;
        }
    }

    let visible = visible_frame.unwrap_or_else(|| {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    });
    let window_frame: NSRect = msg_send![window, frame];
    let origin = NSPoint::new(
        visible.origin.x + (visible.size.width - window_frame.size.width) / 2.0,
        visible.origin.y + (visible.size.height - window_frame.size.height) / 2.0,
    );
    let _: () = msg_send![window, setFrameOrigin: origin];
}

pub(super) fn show_settings() {
    show_settings_inner(None, 0, None, true);
}

/// Present the settings window on `page`. The page is selected (and its pill committed) while the window
/// is still hidden, so the first frame the user sees is already the requested page -- this is the entry
/// every "open settings on page X" caller uses, including the clipboard picker's settings button.
pub(super) fn show_settings_on_page(page: usize) {
    show_settings_inner(None, page, None, true);
}

/// Build the settings window and fill its controls WITHOUT presenting it, so the first real open only has
/// to select a page and order the window front.
///
/// Idempotent by design: a window that already exists -- open, closed by the user, or built by an earlier
/// call -- is left completely alone. Re-entering the load path here would collapse confirmation cards and
/// reset the page the user left the window on.
pub(super) fn prewarm_settings_window() {
    crate::debug_assert_main_thread();
    // `--no-settings-prewarm`: the cold number must stay reproducible by one command, and a scenario that
    // measures the warm path needs the cold path next to it as its counter-example.
    if crate::dev_flags::enabled("no-settings-prewarm") {
        return;
    }
    if with_settings_ui(|ui| ui.is_some()) {
        return;
    }
    // `present_window: false` builds and loads without ordering the window front or switching the app's
    // activation policy; it still runs the layout settle and the content tightening, so the window is
    // final when it is first shown.
    show_settings_inner(None, 0, None, false);
    // A frame carrying `settings_window_built`, so a scenario can wait for the warm-up instead of guessing
    // that it happened (the A2 latency scenario clicks only after this).
    crate::e2e_state::record("settings_prewarmed");
}

/// Reopen Settings from an application-level activation. A visible Settings window keeps its
/// current page; an active Sparkle wait-for-quit flow also retains the existing About surface.
pub(crate) fn reopen_for_app_activation(update_waiting_for_quit: bool) {
    let current_window = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| unsafe {
            let visible: bool = msg_send![ui.window, isVisible];
            let frame: NSRect = msg_send![ui.window, frame];
            (visible, frame, capture_settings_scroll_offsets(ui))
        })
    });

    let preserve_page = update_waiting_for_quit
        || current_window
            .as_ref()
            .is_some_and(|(visible, _, _)| *visible);
    if !preserve_page {
        show_settings();
        return;
    }

    let page = SIDEBAR_SELECTED
        .load(Ordering::SeqCst)
        .min(SETTINGS_PAGE_COUNT - 1);
    if let Some((_, frame, scroll_offsets)) = current_window {
        show_settings_inner(Some(frame), page, Some(scroll_offsets), true);
    } else {
        // An updater wait-for-quit window can exist without a Settings window; About is where
        // the retry action and update status live when Sparkle used the inline host.
        let page = if update_waiting_for_quit {
            SETTINGS_ABOUT_PAGE_INDEX
        } else {
            page
        };
        show_settings_inner(None, page, None, true);
    }
}

/// The sidebar page that was selected when the settings window was last ordered front, for
/// `--e2e-state`. `None` until the window has been presented once in this process.
static PRESENTED_PAGE: AtomicUsize = AtomicUsize::new(usize::MAX);

/// A2 E2E: the page the settings window appeared on, or `None` before its first presentation.
pub(crate) fn e2e_presented_page() -> Option<usize> {
    match PRESENTED_PAGE.load(Ordering::SeqCst) {
        usize::MAX => None,
        page => Some(page),
    }
}

pub(crate) fn settings_window_is_visible() -> bool {
    with_settings_ui(|ui| {
        ui.as_ref().is_some_and(|ui| unsafe {
            let visible: bool = msg_send![ui.window, isVisible];
            visible
        })
    })
}

/// Show the settings window, optionally preserving its frame and selected page.
fn show_settings_preserving(
    frame: NSRect,
    page: usize,
    scroll_offsets: [NSPoint; SETTINGS_PAGE_COUNT],
) {
    show_settings_inner(Some(frame), page, Some(scroll_offsets), false);
}

/// Re-tighten each page document to its real content; returns whether any page changed.
///
/// The build tightens once, but conditional rows (their visibility follows switches and permissions)
/// and the permission banner only expand after the window is *on screen*: the page content then gets
/// taller (measured: +62pt on the clipboard page, exactly one row), and without a second pass that
/// 62pt turns back into dead space below the content.
pub(crate) unsafe fn tighten_page_documents() -> bool {
    let mut changed = false;
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return;
        };
        for (index, scroll) in page_scroll_views(ui).into_iter().enumerate() {
            if scroll.is_null() || !ui.page_canvases[index].is_bound() {
                continue;
            }
            // Each page's layout owner sizes it from its own row list; the only thing that can
            // change behind its back is the viewport (the permission banner resizes General's
            // clip), so hand over the new clip height and let it re-flow if that matters.
            let document: *mut AnyObject = msg_send![scroll, documentView];
            if document.is_null() {
                continue;
            }
            let clip: *mut AnyObject = msg_send![scroll, contentView];
            let clip_bounds: NSRect = msg_send![clip, bounds];
            let before: NSRect = msg_send![document, frame];
            ui.page_canvases[index].set_viewport(clip_bounds.size.height);
            let after: NSRect = msg_send![document, frame];
            if (after.size.height - before.size.height).abs() > 0.5 {
                changed = true;
            }
        }
    });
    changed
}

/// Put a page's clip view at its own top (origin 0), whatever scroll offset an earlier layout
/// pass or a preserved offset left behind.
unsafe fn scroll_page_to_origin_top(scroll: *mut AnyObject) {
    if scroll.is_null() {
        return;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let doc: *mut AnyObject = msg_send![scroll, documentView];
    let doc_frame: NSRect = msg_send![doc, frame];
    let clip_bounds: NSRect = msg_send![clip, bounds];
    // The document is unflipped and the clip is at the TOP when the clip's origin sits at
    // `document height - clip height` -- but AppKit clamps that against the document it has
    // tiled so far. Until the page document is tiled down to the viewport height, that clamp
    // stops short and leaves the page scrolled down; so the document is tiled first.
    let _ = clip_bounds;
    let viewport_h: f64 = {
        let sv: NSRect = msg_send![scroll, frame];
        sv.size.height
    };
    if viewport_h > 0.0 && doc_frame.size.height > viewport_h {
        let _: () = msg_send![clip, setNeedsLayout: true];
        let _: () = msg_send![scroll, layoutSubtreeIfNeeded];
    }
    let doc_after: NSRect = msg_send![doc, frame];
    let origin = NSPoint::new(0.0, (doc_after.size.height - viewport_h).max(0.0));
    let _: () = msg_send![clip, scrollToPoint: origin];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
    let after: NSRect = msg_send![clip, bounds];
    log_debug!(
        "[settings] page top reset: clip_oy={:.1} doc_h={:.1} clip_h={:.1}",
        after.origin.y,
        doc_frame.size.height,
        clip_bounds.size.height
    );
}

#[allow(dead_code)] // kept for a caller that restores a per-page offset; today every open starts at the top
unsafe fn capture_settings_scroll_offsets(ui: &SettingsUi) -> [NSPoint; SETTINGS_PAGE_COUNT] {
    let scrolls = page_scroll_views(ui);
    scrolls.map(|scroll| {
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let bounds: NSRect = msg_send![clip, bounds];
        bounds.origin
    })
}

#[allow(dead_code)] // see capture_settings_scroll_offsets
unsafe fn restore_settings_scroll_offset(scroll: *mut AnyObject, origin: NSPoint) {
    if scroll.is_null() {
        return;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let document: *mut AnyObject = msg_send![scroll, documentView];
    if clip.is_null() || document.is_null() {
        return;
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let document_frame: NSRect = msg_send![document, frame];
    let max_x = (document_frame.size.width - clip_bounds.size.width).max(0.0);
    let max_y = (document_frame.size.height - clip_bounds.size.height).max(0.0);
    let target = NSPoint::new(origin.x.clamp(0.0, max_x), origin.y.clamp(0.0, max_y));
    let _: () = msg_send![clip, scrollToPoint: target];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
}

/// Wall time of the last presenting `show_settings_inner`, in milliseconds (see `e2e_last_open_ms`).
static LAST_OPEN_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The app-side settings-open path, from the click handler's call into the window build/select/compose
/// until the window is ordered front and its page tightened. Excludes the caller's own steps (hiding the
/// picker) and everything the window server does afterwards.
pub(crate) fn e2e_last_open_ms() -> u64 {
    LAST_OPEN_MS.load(Ordering::SeqCst)
}

/// Log one `[settings-timing]` phase at debug level and restart the phase clock.
///
/// The settings open path is on the user's click path (the clipboard picker's gear, the menu item), and its
/// cost is dominated by building the window and filling the mouse page. These lines are the instrument that
/// says whether the window was already warm and which phase still costs; they are cheap and stay in.
fn mark_settings_phase(name: &str, phase: &mut std::time::Instant, total: std::time::Instant) {
    log_debug!(
        "[settings-timing] {name}={}ms total={}ms",
        phase.elapsed().as_millis(),
        total.elapsed().as_millis()
    );
    *phase = std::time::Instant::now();
}

fn show_settings_inner(
    preserved_frame: Option<NSRect>,
    page: usize,
    preserved_scroll_offsets: Option<[NSPoint; SETTINGS_PAGE_COUNT]>,
    present_window: bool,
) {
    let total_started = std::time::Instant::now();
    let mut phase_started = total_started;
    let window_was_built;
    unsafe {
        {
            let missing = with_settings_ui(|ui| ui.is_none());
            window_was_built = missing;
            if missing {
                create_settings_window();
            }
            log_debug!(
                "[settings-timing] open present={present_window} built={missing} page={page}"
            );
            mark_settings_phase("create", &mut phase_started, total_started);
        }
        // Always reopen in the collapsed state so no unfinished confirmation lingers (both cards).
        collapse_restore_confirmations(false);
        // A window order-out is not guaranteed to deliver mouseExited for its tracking areas;
        // clear the active row fill before reusing the settings window.
        widgets::clear_sidebar_hover();
        // The requested page decides whether the mouse page is filled: opening on General or Clipboard
        // must not pay for the mouse page's device resolution and mapping render.
        load_settings_values_for(Some(page));
        mark_settings_phase("load", &mut phase_started, total_started);
        // Normal opens return to General; seamless refreshes preserve the current page. The selection is
        // made HERE, before the window is ordered front, so the first frame the user sees is already the
        // requested page -- presenting first and selecting afterwards showed General and then jumped.
        // Selected before the window is ordered front, so the first frame the user sees is already the
        // requested page. One call is enough: `move_highlight` parks the pill without implicit animation, and
        // this runs before the presentation, so the page is settled by the time the window appears.
        select_sidebar(page);
        mark_settings_phase("select", &mut phase_started, total_started);
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                if present_window {
                    // Switch to .regular so the settings window can activate and raise itself above
                    // the active app; reverted on close.
                    crate::set_settings_activation_policy(true);
                    let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                    let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
                    if let Some(frame) = preserved_frame {
                        // Set the frame while the replacement is hidden so it never visibly jumps from
                        // its default position to the preserved position.
                        let _: () = msg_send![u.window, setFrame: frame, display: false];
                    } else {
                        center_settings_window(u.window);
                    }
                    let _: () =
                        msg_send![u.window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
                    // The page selected when the window actually appeared: a scenario can then assert the
                    // window never showed the wrong page first. The state frame written by `select_sidebar`
                    // is overwritten twice inside one run-loop turn, so the final frame alone cannot tell
                    // the two orders apart.
                    PRESENTED_PAGE.store(SIDEBAR_SELECTED.load(Ordering::SeqCst), Ordering::SeqCst);
                    // Recompute the conditional rows after the window is on screen: AppKit may
                    // re-place subviews by their autoresizing masks the first time a window is
                    // displayed, undoing the compaction done while it was still hidden. The
                    // component derives its state from the live frames, so this call is idempotent
                    // and never shifts twice.
                    update_conditional_rows(u);
                } else if let Some(frame) = preserved_frame {
                    // Theme refresh only replaces content in the same window, preserving its
                    // background order and focus without activating the app.
                    let _: () = msg_send![u.window, setFrame: frame, display: false];
                }
                let _: () = msg_send![u.window, layoutIfNeeded];
                // A preserved offset is only meaningful when the geometry it was read from is
                // unchanged; the page now starts under the title-bar band, so an offset captured
                // by another code path can scroll the page straight past its title. Every open
                // therefore starts the visible page at its top.
                let _ = preserved_scroll_offsets;
                // Scroll the visible page (General on open) to the top after layout, so the
                // scrollbar starts at the top of the track instead of the middle.
                let g = u.page_view(SettingsPageId::General);
                scroll_page_to_top(g);
                scroll_page_to_origin_top(g);
                refresh_settings_warning_banner(u);
            }
        });
        mark_settings_phase("compose", &mut phase_started, total_started);
    }
    if present_window {
        // Clear the default first responder so focus does not land on the Glass color control on open.
        // This runs OUTSIDE the `with_settings_ui` borrow on purpose: ending a field-editing session
        // synchronously calls the text-field delegate (`dispatch::on_control_text_did_end_editing`), which
        // takes that same borrow, and a live RefCell guard across it aborts the process with "RefCell
        // already borrowed". A prewarmed window already holds AppKit's automatic initial first responder
        // (a text field), so on this path the delegate really does fire.
        let window =
            with_settings_ui(|ui| ui.as_ref().map(|u| u.window)).unwrap_or(std::ptr::null_mut());
        if !window.is_null() {
            let _: bool =
                unsafe { msg_send![window, makeFirstResponder: std::ptr::null::<AnyObject>()] };
        }
    }
    // Conditional rows / the permission banner change the content height, so tighten once more; if the
    // height changed, the scroll offset set above is stale, so park the current page back at the top.
    unsafe {
        if tighten_page_documents() {
            let selected = SIDEBAR_SELECTED
                .load(Ordering::SeqCst)
                .min(SETTINGS_PAGE_COUNT - 1);
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    let scrolls = page_scroll_views(u);
                    widgets::scroll_page_to_top(scrolls[selected]);
                }
            });
        }
    }
    mark_settings_phase(
        if window_was_built {
            "done-built"
        } else {
            "done-warm"
        },
        &mut phase_started,
        total_started,
    );
    // A2 E2E: the app's own wall time for this open, published so a scenario can bound the open path
    // without parsing the debug log. Only a presenting call publishes it -- a prewarm is not an open.
    if present_window {
        LAST_OPEN_MS.store(
            total_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            Ordering::SeqCst,
        );
        // Written after the timing is stored: the scenario polls for a frame whose number is this open's.
        crate::e2e_state::record("settings_opened");
    }
}

/// Wrap up every floating editor that belongs to the settings content: the two edit panels and any
/// recording either of them owns. Called before the content hierarchy is hidden, replaced or
/// released, because each panel holds a dim layer parented to a view in that hierarchy.
pub(super) fn close_floating_editors() {
    cancel_recording_from_main();
    close_mapping_panel();
    bindings::close_binding_panel();
}

pub(super) fn hide_settings() {
    // The panel size preview belongs to a live drag on this window: closing Settings must take it
    // down (and restore the panel's window level) even if the drag never produced its mouse-up.
    dispatch::end_max_extent_preview();
    // Defensive: wrap up any in-progress recording and every floating editor when the window closes.
    // The two panels are separate windows that outlive a `hide`, and each holds a dim layer added to
    // the settings content view -- which is what this function is about to stop owning.
    close_floating_editors();
    // A select's panel is a child window and the trigger stays first responder while it is open, so
    // both its state and its focus outlive a hide unless they are wrapped up here.
    super::select::close_open_settings_selects();
    // Closing settings discards any unconfirmed restore action and resets to one button (both
    // cards).
    collapse_restore_confirmations(false);
    let window = with_settings_ui(|ui| ui.as_ref().map(|u| u.window));
    unsafe {
        if let Some(window) = window {
            // orderOut can bypass the sidebar tracking-area exit callback, so clear any active
            // row fill while the window is hidden.
            widgets::clear_sidebar_hover();
            let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    if SYSTEM_APPEARANCE_REBUILD_PENDING.swap(false, Ordering::SeqCst) {
        invalidate_settings_window();
    }
    // Switch back to .accessory: the settings window is closed, return to pure menu-bar (no Dock icon).
    crate::set_settings_activation_policy(false);
}

/// Re-apply the effective appearance from the current CONFIG while preserving page and frame.
///
/// Custom settings layers are painted with concrete palette colors at construction time. When
/// the window is visible, remember the current page and frame, clear and rebuild the content
/// hierarchy inside the same NSWindow, then restore the page and frame. This avoids the visible
/// order-out/reappear gap and the mixed state caused by updating only NSWindow.appearance.
/// Immediate-apply writes CONFIG before calling this, so the redraw already shows the new values.
pub(crate) fn refresh_system_appearance() {
    let (old_window, visible) = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| unsafe {
            let visible: bool = msg_send![ui.window, isVisible];
            (ui.window, visible)
        })
    })
    .unwrap_or((std::ptr::null_mut(), false));

    if old_window.is_null() || !visible {
        invalidate_settings_window();
        return;
    }

    let (page, frame, scroll_offsets) = {
        let page = SIDEBAR_SELECTED.load(Ordering::SeqCst);
        let frame: NSRect = unsafe { msg_send![old_window, frame] };
        let scroll_offsets = with_settings_ui(|ui| {
            ui.as_ref()
                .map(|ui| unsafe { capture_settings_scroll_offsets(ui) })
        })
        .unwrap_or([NSPoint::new(0.0, 0.0); SETTINGS_PAGE_COUNT]);
        (page, frame, scroll_offsets)
    };

    let old_ui = with_settings_ui(|ui| ui.take());
    let Some(old_ui) = old_ui else {
        invalidate_settings_window();
        return;
    };

    unsafe {
        // Detach callbacks and registries, then redraw the content hierarchy inside the same
        // visible NSWindow. The window never orders out, changes identity, or changes focus.
        // Every floating editor goes first: each holds a dim layer parented to the content view that
        // is about to be rebuilt, so leaving one open would leave it pointing at a freed view.
        close_floating_editors();
        widgets::clear_sidebar_hover();
        detach_settings_window_runtime();
        rebuild_settings_content(old_ui.window);
        show_settings_preserving(frame, page, scroll_offsets);
    }
}

/// The About page's document height plus every child's top edge, in child order. The inline update
/// flow must move the page as one block and a collapse must put everything back, so the checks
/// compare these snapshots instead of trusting a single view.
struct AboutPanelSnapshot {
    document_height: f64,
    child_tops: Vec<f64>,
}

unsafe fn about_panel_snapshot() -> Option<AboutPanelSnapshot> {
    with_settings_ui(|ui| {
        let ui = ui.as_ref()?;
        let document: *mut AnyObject = msg_send![ui.page_view(SettingsPageId::About), documentView];
        if document.is_null() {
            return None;
        }
        let doc_frame: NSRect = msg_send![document, frame];
        let subviews: *mut AnyObject = msg_send![document, subviews];
        let count: usize = msg_send![subviews, count];
        let mut child_tops = Vec::with_capacity(count);
        for index in 0..count {
            let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
            if child.is_null() {
                return None;
            }
            // Every child is kept, including the zero-height compact update host, so the indices
            // line up across snapshots (the expand/collapse only resize views, never add or remove).
            let frame: NSRect = msg_send![child, frame];
            child_tops.push(frame.origin.y + frame.size.height);
        }
        Some(AboutPanelSnapshot {
            document_height: doc_frame.size.height,
            child_tops,
        })
    })
}

/// Whether a collapse restored a snapshot exactly: same document height, every child back in place.
fn page_restored(before: &AboutPanelSnapshot, after: &AboutPanelSnapshot) -> bool {
    if before.child_tops.len() != after.child_tops.len() {
        return false;
    }
    (after.document_height - before.document_height).abs() <= 1.0
        && before
            .child_tops
            .iter()
            .zip(after.child_tops.iter())
            .all(|(before, after)| (after - before).abs() <= 1.0)
}

/// Whether a page shows `text` as an actually-visible label.
///
/// A `described` row's subtitle is never rendered, so a consequence line handed to one silently
/// does not exist. Both captions that carry a promise the user cannot otherwise see (the clipboard
/// disable warning, the import overwrite warning) are asserted this way, against the real view tree
/// rather than the call that was supposed to draw them.
unsafe fn page_shows_visible_caption(page: *mut AnyObject, expected: &str) -> bool {
    fn find(view: *mut AnyObject, expected: &str) -> bool {
        unsafe {
            if view.is_null() {
                return false;
            }
            let is_text_field: bool = msg_send![view, isKindOfClass: class!(NSTextField)];
            if is_text_field {
                let hidden: bool = msg_send![view, isHidden];
                let text: *mut AnyObject = msg_send![view, stringValue];
                if !hidden && crate::ffi::nsstring_to_rust(text) == expected {
                    let frame: NSRect = msg_send![view, frame];
                    return frame.size.width > 0.0 && frame.size.height > 0.0;
                }
            }
            let children: *mut AnyObject = msg_send![view, subviews];
            if children.is_null() {
                return false;
            }
            let count: usize = msg_send![children, count];
            (0..count).any(|index| {
                let child: *mut AnyObject = msg_send![children, objectAtIndex: index];
                find(child, expected)
            })
        }
    }
    find(page, expected)
}

unsafe fn settings_style_tree_is_valid(
    view: *mut AnyObject,
    inside_control: bool,
    page_name: &str,
) -> bool {
    if view.is_null() {
        return true;
    }
    let is_control: bool = msg_send![view, isKindOfClass: class!(NSControl)];
    let is_text_field: bool = msg_send![view, isKindOfClass: class!(NSTextField)];
    let is_button: bool = msg_send![view, isKindOfClass: class!(NSButton)];
    let is_slider: bool = msg_send![view, isKindOfClass: class!(NSSlider)];
    if is_slider {
        let ticks: isize = msg_send![view, numberOfTickMarks];
        if ticks != 0 {
            // Tick marks are forbidden except on a stepped slider, and there only when AppKit is
            // doing the snapping and the marks divide the range into whole equal steps: a
            // tick-marked slider free to rest between the stops it draws is what the rule guards
            // against.
            let allows: bool = msg_send![view, allowsTickMarkValuesOnly];
            let identifier: *mut AnyObject = msg_send![view, identifier];
            let is_stepped = !identifier.is_null()
                && crate::ffi::nsstring_to_rust(identifier) == widgets::STEPPED_SLIDER_IDENTIFIER;
            let min: f64 = msg_send![view, minValue];
            let max: f64 = msg_send![view, maxValue];
            let step = (max - min) / (ticks as f64 - 1.0);
            let whole_steps = step > 0.0 && (step - step.round()).abs() < 1e-9;
            if !(is_stepped && allows && whole_steps) {
                log_info!(
                    "[smoke-settings-layout] slider has {ticks} tick marks (stepped={is_stepped}, snaps={allows}, step={step:.3})"
                );
                return false;
            }
        }
    }
    let draws_text = if is_text_field {
        let text: *mut AnyObject = msg_send![view, stringValue];
        let editable: bool = msg_send![view, isEditable];
        !editable && !crate::ffi::nsstring_to_rust(text).is_empty()
    } else if is_button {
        let title: *mut AnyObject = msg_send![view, title];
        !crate::ffi::nsstring_to_rust(title).is_empty()
    } else {
        false
    };
    if draws_text && (!inside_control || super::select::is_registered_select_label(view)) {
        let invalid_size = settings_rendered_font_sizes(view, is_text_field)
            .into_iter()
            .find(|size| {
                ![
                    crate::theme::FONT_CAPTION,
                    crate::theme::FONT_CONTROL,
                    crate::theme::FONT_SIDEBAR_TITLE,
                    crate::theme::FONT_PAGE_TITLE,
                ]
                .into_iter()
                .any(|token| (size - token).abs() <= 0.1)
            });
        if let Some(size) = invalid_size {
            let tag = if is_control {
                msg_send![view, tag]
            } else {
                -1isize
            };
            let value = if is_text_field {
                let string: *mut AnyObject = msg_send![view, stringValue];
                crate::ffi::nsstring_to_rust(string)
            } else if is_button {
                let title: *mut AnyObject = msg_send![view, title];
                crate::ffi::nsstring_to_rust(title)
            } else {
                String::new()
            };
            let class: *const AnyClass = msg_send![view, class];
            let class_name = class
                .as_ref()
                .map(|class| class.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| "<unknown>".to_string());
            let frame: NSRect = msg_send![view, frame];
            log_info!(
                "[smoke-settings-layout] unexpected rendered font size {size:.2}, page={page_name}, class={class_name}, control={is_control}, text={is_text_field}, button={is_button}, tag={tag}, value={value:?}, frame={frame:?}"
            );
            return false;
        }
    }
    let identifier: *mut AnyObject = msg_send![view, identifier];
    if !identifier.is_null()
        && crate::ffi::nsstring_to_rust(identifier) == widgets::SETTINGS_CARD_STYLE_IDENTIFIER
    {
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return false;
        }
        let opacity: f32 = msg_send![layer, shadowOpacity];
        if opacity > 0.0 {
            log_info!("[smoke-settings-layout] settings card has shadow opacity={opacity:.3}");
            return false;
        }
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return true;
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if !settings_style_tree_is_valid(child, inside_control || is_control, page_name) {
            return false;
        }
    }
    true
}

/// The slider rows whose setting is a whole number of units must be stepped: a continuous control
/// can rest between two values while the readout and the persisted field truncate to one of them.
/// The style-tree walk only proves that a *ticked* slider is stepped, so this pins the control kind
/// at the rows that need it -- a re-introduced plain integer slider fails this smoke.
unsafe fn integer_sliders_are_stepped() -> bool {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return false;
        };
        [
            ui.card_text_size,
            ui.status_bar_text_size,
            ui.line_count,
            ui.clipboard_auto_expire_days,
        ]
        .into_iter()
        .all(|slider| {
            if slider.is_null() {
                return false;
            }
            let identifier: *mut AnyObject = msg_send![slider, identifier];
            let is_stepped = !identifier.is_null()
                && crate::ffi::nsstring_to_rust(identifier) == widgets::STEPPED_SLIDER_IDENTIFIER;
            is_stepped && {
                let allows: bool = msg_send![slider, allowsTickMarkValuesOnly];
                allows
            }
        })
    })
}

unsafe fn settings_rendered_font_sizes(view: *mut AnyObject, is_text_field: bool) -> Vec<f64> {
    let mut sizes = Vec::new();
    let mut uses_control_font = true;
    if is_text_field {
        let attributed: *mut AnyObject = msg_send![view, attributedStringValue];
        let length: usize = msg_send![attributed, length];
        if !attributed.is_null() && length > 0 {
            let mut index = 0usize;
            uses_control_font = false;
            while index < length {
                let mut effective_range = NSRange::new(0, 0);
                let attributes: *mut AnyObject = msg_send![
                    attributed,
                    attributesAtIndex: index,
                    effectiveRange: &mut effective_range as *mut NSRange
                ];
                let font: *mut AnyObject = if attributes.is_null() {
                    std::ptr::null_mut()
                } else {
                    msg_send![attributes, objectForKey: widgets::NSFontAttributeName]
                };
                if font.is_null() {
                    uses_control_font = true;
                } else {
                    sizes.push(msg_send![font, pointSize]);
                }
                let next = effective_range
                    .location
                    .saturating_add(effective_range.length)
                    .min(length);
                if next <= index {
                    uses_control_font = true;
                    break;
                }
                index = next;
            }
        }
    }

    if uses_control_font || sizes.is_empty() {
        let responds_to_font: bool = msg_send![view, respondsToSelector: sel!(font)];
        if responds_to_font {
            let font: *mut AnyObject = msg_send![view, font];
            if !font.is_null() {
                sizes.push(msg_send![font, pointSize]);
            }
        }
    }
    sizes
}

unsafe fn settings_about_icon_is_aligned(view: *mut AnyObject) -> bool {
    if view.is_null() {
        return false;
    }
    let identifier: *mut AnyObject = msg_send![view, identifier];
    if crate::ffi::nsstring_to_rust(identifier) == ABOUT_APP_ICON_IDENTIFIER {
        let is_image_view: bool = msg_send![view, isKindOfClass: class!(NSImageView)];
        let image: *mut AnyObject = msg_send![view, image];
        let frame: NSRect = msg_send![view, frame];
        let slot: *mut AnyObject = msg_send![view, superview];
        if slot.is_null() {
            return false;
        }
        let slot_frame: NSRect = msg_send![slot, frame];
        let visible_leading_x = slot_frame.origin.x + frame.origin.x;
        return is_image_view
            && !image.is_null()
            && frame.size.width > 0.0
            && frame.size.height > 0.0
            && (visible_leading_x - ABOUT_HEADER_CONTENT_LEADING_X).abs() <= 0.5;
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return false;
    }
    let count: usize = msg_send![subviews, count];
    (0..count).any(|index| {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        settings_about_icon_is_aligned(child)
    })
}

unsafe fn settings_about_title_is_product_name(view: *mut AnyObject) -> bool {
    if view.is_null() {
        return false;
    }
    if msg_send![view, isKindOfClass: class!(NSTextField)] {
        let value: *mut AnyObject = msg_send![view, stringValue];
        if crate::ffi::nsstring_to_rust(value) == "Oh My Tab" {
            return true;
        }
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return false;
    }
    let count: usize = msg_send![subviews, count];
    (0..count).any(|index| {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        settings_about_title_is_product_name(child)
    })
}

unsafe fn settings_sidebar_has_redundant_heading(view: *mut AnyObject) -> bool {
    if view.is_null() {
        return false;
    }
    if msg_send![view, isKindOfClass: class!(NSTextField)] {
        let value: *mut AnyObject = msg_send![view, stringValue];
        if crate::ffi::nsstring_to_rust(value) == t("settings.window_title") {
            return true;
        }
    }
    let subviews: *mut AnyObject = msg_send![view, subviews];
    if subviews.is_null() {
        return false;
    }
    let count: usize = msg_send![subviews, count];
    (0..count).any(|index| {
        let child: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        settings_sidebar_has_redundant_heading(child)
    })
}

/// Whether the General page's visible card titles start with Startup, then Menu bar icon, then
/// Appearance -- the leading order the page promises.
///
/// Matched on the rendered header labels, in the document's own coordinate space (AppKit's y grows
/// upward, so "first" is the largest y), which is what the user actually sees. A card move that
/// skips this order is invisible to every other smoke check here.
unsafe fn general_page_leads_with_startup_and_icon_cards() -> bool {
    /// Walk a view tree, recording each visible label with its frame in the *page's* coordinate
    /// space. The page is the root passed in and every card label is a descendant, so converting
    /// each label's bounds to that root gives one comparable space for all of them.
    fn collect(view: *mut AnyObject, root: *mut AnyObject, out: &mut Vec<(f64, String)>) {
        unsafe {
            if view.is_null() {
                return;
            }
            let is_text_field: bool = msg_send![view, isKindOfClass: class!(NSTextField)];
            if is_text_field {
                let hidden: bool = msg_send![view, isHidden];
                let text: *mut AnyObject = msg_send![view, stringValue];
                if !hidden {
                    // The card labels are laid out in the page document's own coordinate space, and
                    // the settings view tree is not flipped relative to it, so the raw frame origin
                    // is already the position to compare (reading it through convertRect:toView:
                    // returned the flipped space and inverted the order). `root` is kept for the
                    // recursion signature only.
                    let _ = root;
                    let frame: NSRect = msg_send![view, frame];
                    out.push((frame.origin.y, crate::ffi::nsstring_to_rust(text)));
                }
            }
            let children: *mut AnyObject = msg_send![view, subviews];
            if children.is_null() {
                return;
            }
            let count: usize = msg_send![children, count];
            for index in 0..count {
                let child: *mut AnyObject = msg_send![children, objectAtIndex: index as isize];
                collect(child, root, out);
            }
        }
    }
    // The card headers are rendered by the shared card component as plain labels; matching on the
    // text keeps this independent of the component's internals. Each key is unique to one card on
    // this page, so the label identifies the card.
    let general =
        with_settings_ui(|ui| ui.as_ref().map(|ui| ui.page_view(SettingsPageId::General)));
    let Some(general) = general else {
        return false;
    };
    let mut labels = Vec::new();
    collect(general, general, &mut labels);
    // Check the whole leading sequence, not just the relative order of a few labels: a card moved
    // above Startup, or inserted between the first two, would satisfy a pairwise comparison while
    // breaking the promise. Only card headers are collected, so the page title and the rows inside
    // each card cannot disturb the sequence. AppKit's y grows upward, so the largest y is first.
    let mut headers: Vec<(f64, String)> = [
        "settings.header_startup",
        "settings.header_menu_bar_icon",
        "settings.header_appearance",
        "settings.header_language",
        "settings.header_logging",
        "settings.header_backup",
    ]
    .into_iter()
    .filter_map(|key| {
        let wanted = t(key);
        labels
            .iter()
            .find(|(_, text)| *text == wanted)
            .map(|(y, _)| (*y, wanted))
    })
    .collect();
    if headers.len() < 4 {
        log_info!(
            "[smoke-settings-layout] only {} card headers found; cannot check the leading order",
            headers.len()
        );
        return false;
    }
    headers.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let order: Vec<&str> = headers.iter().map(|(_, text)| text.as_str()).collect();
    // The page's first four cards, top to bottom, must be exactly: startup, menu-bar icon,
    // appearance, language. This catches both a swap and an insertion before or between them.
    order[0] == t("settings.header_startup")
        && order[1] == t("settings.header_menu_bar_icon")
        && order[2] == t("settings.header_appearance")
        && order[3] == t("settings.header_language")
}
/// Whether the expanded update card and its host sit inside the page document with the page's bottom
/// padding, and the document covers the card's top edge (i.e. the whole card can be scrolled to).
unsafe fn update_card_inside_document() -> bool {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return false;
        };
        let document: *mut AnyObject = msg_send![ui.page_view(SettingsPageId::About), documentView];
        if document.is_null() {
            return false;
        }
        let doc_frame: NSRect = msg_send![document, frame];
        let card: NSRect = msg_send![ui.update_card, frame];
        let host: NSRect = msg_send![ui.update_host, frame];
        let ok = card.origin.y >= super::UPDATE_CARD_BOTTOM_PADDING - 0.5
            && host.origin.y >= super::UPDATE_CARD_BOTTOM_PADDING - 0.5
            && doc_frame.size.height + 0.5 >= card.origin.y + card.size.height;
        if !ok {
            log_info!(
                "[smoke-settings-layout] update card outside the page: doc={:.1} card={:?} host={:?} padding={}",
                doc_frame.size.height,
                card,
                host,
                super::UPDATE_CARD_BOTTOM_PADDING
            );
        }
        ok
    })
}

/// Whether a page's clip view is parked at its document's top, read from the live frames.
///
/// Fails when a reflow grows the document without moving the clip: the mouse page's
/// materialize-after-scroll case, exercised here on the About page's update expansion.
unsafe fn page_is_at_top(scroll: *mut AnyObject) -> bool {
    if scroll.is_null() {
        return false;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let document: *mut AnyObject = msg_send![scroll, documentView];
    if clip.is_null() || document.is_null() {
        return false;
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let document_frame: NSRect = msg_send![document, frame];
    let top = (document_frame.size.height - clip_bounds.size.height).max(0.0);
    (clip_bounds.origin.y - top).abs() <= 0.5
}

/// Whether a custom switch's knob layer is mid-transition (has a running animation). Used to prove
/// the mouse page's lazy fill does not replay a state transition on an already visible page.
unsafe fn switch_is_animating(button: *mut AnyObject) -> bool {
    if button.is_null() {
        return false;
    }
    let layer: *mut AnyObject = msg_send![button, layer];
    if layer.is_null() {
        return false;
    }
    let sublayers: *mut AnyObject = msg_send![layer, sublayers];
    if sublayers.is_null() {
        return false;
    }
    let count: usize = msg_send![sublayers, count];
    if count == 0 {
        return false;
    }
    let knob: *mut AnyObject = msg_send![sublayers, objectAtIndex: 0usize];
    let keys: *mut AnyObject = msg_send![knob, animationKeys];
    if keys.is_null() {
        return false;
    }
    let key_count: usize = msg_send![keys, count];
    key_count > 0
}

/// The inline update flow's ownership state as the layout leaves it: what the flow itself hid,
/// where its divider sits relative to the row above it, and how the card hugs the host.
#[derive(Debug)]
struct UpdateFlowState {
    document_height: f64,
    button_hidden: bool,
    host_hidden: bool,
    divider_from_row_above: f64,
    row_control_height: f64,
    card_rect_bottom_from_host_bottom: f64,
    /// The restore surface's offset from its container: the two are document siblings, so a re-flow
    /// has to move them together or the surface drifts out from under the control.
    restore_surface_from_container: f64,
    /// The restore control's own expanded state, which a re-flow must not disturb either.
    restore_expanded: bool,
}

unsafe fn update_flow_state() -> Option<UpdateFlowState> {
    with_settings_ui(|ui| {
        let ui = ui.as_ref()?;
        if ui.update_card.is_null() || ui.update_host.is_null() || ui.update_divider.is_null() {
            return None;
        }
        let document: *mut AnyObject = msg_send![ui.page_view(SettingsPageId::About), documentView];
        let doc_frame: NSRect = msg_send![document, frame];
        // The canvas sets a card's frame to its visible rect, so `origin.y` is the card's bottom.
        let card: NSRect = msg_send![ui.update_card, frame];
        let host: NSRect = msg_send![ui.update_host, frame];
        let divider: NSRect = msg_send![ui.update_divider, frame];
        let row_above: NSRect = msg_send![ui.update_auto_download, frame];
        let button_hidden: bool = msg_send![ui.update_check_button, isHidden];
        let host_hidden: bool = msg_send![ui.update_host, isHidden];
        let container_frame: NSRect =
            msg_send![ui.page_restores[SETTINGS_ABOUT_PAGE_INDEX].container, frame];
        let surface_frame: NSRect =
            msg_send![ui.page_restores[SETTINGS_ABOUT_PAGE_INDEX].surface, frame];
        Some(UpdateFlowState {
            document_height: doc_frame.size.height,
            button_hidden,
            host_hidden,
            divider_from_row_above: divider.origin.y - row_above.origin.y,
            row_control_height: row_above.size.height,
            card_rect_bottom_from_host_bottom: card.origin.y - host.origin.y,
            restore_surface_from_container: surface_frame.origin.y - container_frame.origin.y,
            restore_expanded: ui.page_restores[SETTINGS_ABOUT_PAGE_INDEX].expanded,
        })
    })
}

/// Run every settings page through the real AppKit layout path and debug validator, then exit.
/// This is used by the ignored macOS smoke test; it deliberately exercises the same window
/// builder as the interactive app instead of constructing a simplified test-only hierarchy.
pub(crate) fn settings_layout_smoke_runner() -> bool {
    const UPDATE_CARD_EXPANSION_REQUEST: f64 = 640.0;
    // The page layout reserves part of the requested host height for the row and bottom padding;
    // keep the existing 500pt document-growth floor and request enough content height to clear it.
    const MIN_UPDATE_DOCUMENT_GROWTH: f64 = 500.0;
    unsafe {
        if !widgets::smoke_text_field_line_fragments() {
            log_info!("[smoke-settings-layout] NSTextField line-fragment smoke failed");
            return false;
        }
        log_info!("[smoke-settings-layout] opening settings");
        show_settings();
        log_info!("[smoke-settings-layout] settings opened");
        // The engine's "is this app frontmost" flag drives a pass-through rule for window actions
        // (see `key_bindings::engine`). Two halves are assertable here; the third is not, and is
        // recorded in `docs/review-backlog.md` rather than faked:
        //   * the controller class really answers both activation selectors (a missing or renamed
        //     registration would leave the flag permanently false);
        //   * publishing and reading the flag round-trips.
        // What a smoke run cannot produce is the notification itself: this process is not the active
        // application during a smoke run, so AppKit never delivers it. Asserting a value here would
        // be asserting `false == false` and would pass with the publication deleted.
        if !bindings::debug_validate_self_frontmost_wiring() {
            hide_settings();
            return false;
        }
        // The select's open surface and focus hand-back are only observable with a real window.
        if !super::select::settings_select_focus_ring_smoke() {
            log_info!("[smoke-settings-layout] select open/close focus smoke failed");
            return false;
        }
        // Regression guard for the guide's "Open App Settings" crash (2026-09-28): the settings
        // window becoming key delivers the scroller notification synchronously from inside
        // `with_settings_ui`, and the resync re-entered the borrow ("RefCell already borrowed" ->
        // abort). Driving the real handler with an active borrow must defer, not panic; without the
        // guard this kills the smoke process and the test fails.
        {
            let controller = crate::CONTROLLER.lock().unwrap().map(|target| target.0);
            let Some(controller) = controller else {
                log_info!(
                    "[smoke-settings-layout] no controller for the scroller-notification smoke"
                );
                hide_settings();
                return false;
            };
            let settings_window = crate::settings::settings_window_ptr();
            if settings_window.is_null() {
                log_info!(
                    "[smoke-settings-layout] no settings window for the scroller-notification smoke"
                );
                hide_settings();
                return false;
            }
            // A notification delivered synchronously from inside the borrow must defer (a
            // nested settings borrow used to abort the process).
            with_settings_ui(|_| {
                crate::on_scroller_style_changed(
                    controller,
                    sel!(handleScrollerStyleChanged:),
                    std::ptr::null_mut(),
                );
            });
            // The deferred call arrives with no notification object, exactly like this direct
            // one: messaging that nil note (the diagnostic) used to panic inside an `extern "C"`
            // callback and abort. Both paths must survive, and an object-less note must still reach
            // the resync (the scroller style can change while the settings window is already key).
            let before_null = crate::scroller::resync_entries();
            crate::on_scroller_style_changed(
                controller,
                sel!(handleScrollerStyleChanged:),
                std::ptr::null_mut(),
            );
            let null_ok = crate::scroller::resync_entries() == before_null + 1;
            // A window-did-become-key notification for any window that is not the settings window --
            // the switcher panel is the real one -- must be filtered out: it used to rebuild the
            // whole settings content synchronously, stalling the overlay's summon behind it.
            let become_key_name = make_nsstring("NSWindowDidBecomeKeyNotification");
            let foreign_window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
            let foreign_window: *mut AnyObject = msg_send![foreign_window,
                initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(80.0, 80.0)),
                styleMask: 0u64,
                backing: 2u64,
                defer: false
            ];
            if foreign_window.is_null() {
                log_info!(
                    "[smoke-settings-layout] could not create a foreign window for the scroller-notification smoke"
                );
                CFRelease(become_key_name as *const c_void);
                hide_settings();
                return false;
            }
            let foreign_note: *mut AnyObject = msg_send![
                class!(NSNotification),
                notificationWithName: become_key_name,
                object: foreign_window
            ];
            let before_foreign = crate::scroller::resync_entries();
            crate::on_scroller_style_changed(
                controller,
                sel!(handleScrollerStyleChanged:),
                foreign_note,
            );
            let foreign_ok = crate::scroller::resync_entries() == before_foreign;
            // Positive control: the settings window's own become-key must still resync, or an
            // implementation that early-returns for *every* window would pass the check above.
            let settings_note: *mut AnyObject = msg_send![
                class!(NSNotification),
                notificationWithName: become_key_name,
                object: settings_window
            ];
            let before_settings = crate::scroller::resync_entries();
            crate::on_scroller_style_changed(
                controller,
                sel!(handleScrollerStyleChanged:),
                settings_note,
            );
            let settings_ok = crate::scroller::resync_entries() == before_settings + 1;
            release_obj(foreign_window);
            CFRelease(become_key_name as *const c_void);
            // The app's activation notification shares this handler and carries the application, not
            // a window: it must still resync.
            let active_name = make_nsstring("NSApplicationDidBecomeActiveNotification");
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let active_note: *mut AnyObject = msg_send![
                class!(NSNotification),
                notificationWithName: active_name,
                object: app
            ];
            let before_app = crate::scroller::resync_entries();
            crate::on_scroller_style_changed(
                controller,
                sel!(handleScrollerStyleChanged:),
                active_note,
            );
            let app_ok = crate::scroller::resync_entries() == before_app + 1;
            CFRelease(active_name as *const c_void);
            if !(null_ok && foreign_ok && settings_ok && app_ok) {
                log_info!(
                    "[smoke-settings-layout] scroller notification filter wrong: object_less={null_ok} foreign={foreign_ok} settings={settings_ok} app={app_ok}"
                );
                hide_settings();
                return false;
            }
        }
        let Some((window, pages)) =
            with_settings_ui(|ui| ui.as_ref().map(|ui| (ui.window, page_scroll_views(ui))))
        else {
            return false;
        };
        // Keep the custom title bar band clear of the native AppKit window buttons.
        struct TitlebarCheck {
            found: bool,
            band: NSRect,
            host: NSRect,
            sidebar_top: f64,
            traffic_lights_ok: bool,
        }
        let titlebar = with_settings_ui(|ui| {
            let zero = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            let mut check = TitlebarCheck {
                found: false,
                band: zero,
                host: zero,
                sidebar_top: f64::NAN,
                traffic_lights_ok: false,
            };
            let Some(ui) = ui.as_ref() else {
                return check;
            };
            let host: *mut AnyObject = msg_send![window, contentView];
            if host.is_null() {
                return check;
            }
            // The band is a child of the custom root surface (the settings content), not of AppKit's
            // content host: the root is what holds every surface the window paints.
            let root = settings_root_view_for_host(host);
            let band = settings_titlebar_view_for_host(root);
            if band.is_null() {
                return check;
            }
            check.found = true;
            check.band = msg_send![band, frame];
            check.host = msg_send![host, bounds];
            let sidebar_content: *mut AnyObject =
                msg_send![sidebar_buttons(ui)[SETTINGS_GENERAL_PAGE_INDEX], superview];
            if !sidebar_content.is_null() {
                let sidebar_frame: NSRect = msg_send![sidebar_content, frame];
                check.sidebar_top = sidebar_frame.origin.y + sidebar_frame.size.height;
            }
            check.traffic_lights_ok = traffic_light_buttons(window).into_iter().all(|button| {
                if button.is_null() {
                    return false;
                }
                let hidden: bool = msg_send![button, isHiddenOrHasHiddenAncestor];
                let alpha: f64 = msg_send![button, alphaValue];
                let bounds: NSRect = msg_send![button, bounds];
                let frame: NSRect = msg_send![button, convertRect: bounds, toView: host];
                !hidden
                    && alpha > 0.0
                    && frame.size.width > 0.0
                    && frame.size.height > 0.0
                    && frame.origin.x >= 0.0
                    && frame.origin.x + frame.size.width <= check.host.size.width
                    && frame.origin.y >= check.host.size.height - TITLEBAR_H
                    && frame.origin.y + frame.size.height <= check.host.size.height
            });
            check
        });
        let titlebar_ok = titlebar.found
            && titlebar.traffic_lights_ok
            && (titlebar.band.size.height - TITLEBAR_H).abs() <= 0.5
            && (titlebar.band.origin.y + titlebar.band.size.height - titlebar.host.size.height)
                .abs()
                <= 0.5
            && titlebar.sidebar_top <= titlebar.host.size.height - TITLEBAR_H + 0.5;
        if !titlebar_ok {
            log_info!(
                "[smoke-settings-layout] title bar band does not match the mock: found={} band={:?} host={:?} sidebar_top={:.1} lights={}",
                titlebar.found,
                titlebar.band,
                titlebar.host,
                titlebar.sidebar_top,
                titlebar.traffic_lights_ok
            );
            hide_settings();
            return false;
        }
        let sidebar_heading_absent = with_settings_ui(|ui| {
            let Some(ui) = ui.as_ref() else {
                return false;
            };
            let sidebar_content: *mut AnyObject =
                msg_send![ui.sidebar_button(SettingsPageId::General), superview];
            !sidebar_content.is_null() && !settings_sidebar_has_redundant_heading(sidebar_content)
        });
        if !sidebar_heading_absent {
            log_info!("[smoke-settings-layout] redundant sidebar heading is still present");
            hide_settings();
            return false;
        }
        if !settings_about_icon_is_aligned(pages[SETTINGS_ABOUT_PAGE_INDEX]) {
            log_info!("[smoke-settings-layout] About app icon is missing or misaligned");
            hide_settings();
            return false;
        }
        if !settings_about_title_is_product_name(pages[SETTINGS_ABOUT_PAGE_INDEX]) {
            log_info!("[smoke-settings-layout] About title is not the product name");
            hide_settings();
            return false;
        }
        // One name per page, in sidebar order, from the page table the sidebar itself is built from.
        if pages.iter().enumerate().any(|(index, page)| {
            !settings_style_tree_is_valid(*page, false, settings_page_name(index))
        }) {
            log_info!(
                "[smoke-settings-layout] font tokens, slider ticks, or card elevation are invalid"
            );
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] style tree passed");
        if !integer_sliders_are_stepped() {
            log_info!("[smoke-settings-layout] an integer slider does not snap to whole units");
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] integer sliders are stepped");
        // The clipboard switch deletes the saved history when it goes off, so the page must SAY so:
        // its consequence line has to be a visible label in the real view tree. It was previously
        // handed to a `described` row, whose subtitle this app never renders, and the warning
        // silently did not exist.
        if !page_shows_visible_caption(pages[3], &t("settings.desc_clipboard_enabled")) {
            log_info!("[smoke-settings-layout] the clipboard disable warning is not visible");
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] clipboard disable warning visible");
        // Same requirement for the import row: the confirmation dialog was deliberately dropped, so
        // the row's own warning line is the only thing telling the user the import overwrites their
        // settings. It has to be a visible label, and the General page is index 0.
        for key in [
            "settings.desc_import_settings",
            "settings.desc_export_settings",
        ] {
            if !page_shows_visible_caption(pages[0], &t(key)) {
                log_info!("[smoke-settings-layout] backup caption {key} is not visible");
                hide_settings();
                return false;
            }
        }
        log_info!("[smoke-settings-layout] backup captions visible");
        // Hiding the menu-bar icon removes the app's only always-present entry point, so the row's
        // description is what tells the user Settings can still be reached. It sits on a `described`
        // row, whose subtitle this app never renders -- exactly the shape that silently lost the
        // clipboard warning above -- so the promise has to be asserted on the real view tree.
        if !page_shows_visible_caption(pages[0], &t("settings.desc_hide_menu_bar_icon")) {
            log_info!("[smoke-settings-layout] the hide-menu-bar-icon explanation is not visible");
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] hide-menu-bar-icon explanation visible");
        // Startup is the General page's first card and the menu-bar icon its own card directly
        // below: the ordering is what the user asked for, and a card move is invisible to every
        // other check here.
        if !general_page_leads_with_startup_and_icon_cards() {
            log_info!(
                "[smoke-settings-layout] the General page does not lead with the startup then icon cards"
            );
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] General page leads with the startup and icon cards");
        // The hover tooltip is the outlet for a caption that cannot fit its row (design-style §9).
        // Assert it on the real view tree: the bubble must render the longest shipped caption in full,
        // not truncate it a second time -- the fixed 288pt bubble with its 220pt text budget did
        // exactly that for the English and Traditional-Chinese "delete after paste" captions.
        if !tooltip::SettingsTooltip::debug_hover_bubble_holds_captions(
            window,
            &[
                t("settings.desc_clipboard_enabled"),
                t("settings.desc_clipboard_delete_after_paste"),
                t("settings.desc_clipboard_shortcut"),
            ],
        ) {
            log_info!("[smoke-settings-layout] the hover tooltip truncates its own text");
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] hover tooltip renders its captions in full");
        let sidebar_layout_ok = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let buttons = sidebar_buttons(ui);
            if buttons.iter().any(|button| button.is_null()) {
                return Some(false);
            }
            let parent: *mut AnyObject = msg_send![buttons[0], superview];
            if parent.is_null() {
                return Some(false);
            }
            let selected = SIDEBAR_SELECTED.load(Ordering::SeqCst);
            let hover_probe = buttons.iter().copied().find(|button| {
                let tag: isize = msg_send![*button, tag];
                tag >= 0 && tag as usize != selected
            });
            if !hover_probe.is_some_and(|button| widgets::sidebar_hover_style_smoke(button)) {
                return Some(false);
            }
            let bounds: NSRect = msg_send![parent, bounds];
            let mut previous: Option<NSRect> = None;
            for button in buttons {
                let button_parent: *mut AnyObject = msg_send![button, superview];
                let frame: NSRect = msg_send![button, frame];
                if button_parent != parent
                    || frame.size.width <= 0.0
                    || frame.size.height <= 0.0
                    || frame.origin.x < bounds.origin.x
                    || frame.origin.y < bounds.origin.y
                    || frame.origin.x + frame.size.width > bounds.origin.x + bounds.size.width
                    || frame.origin.y + frame.size.height > bounds.origin.y + bounds.size.height
                {
                    return Some(false);
                }
                if let Some(previous_frame) = previous {
                    let step = previous_frame.origin.y - frame.origin.y;
                    if (step - (previous_frame.size.height + components::SIDEBAR_ROW_GAP)).abs()
                        > 0.5
                    {
                        return Some(false);
                    }
                }
                previous = Some(frame);
            }
            let lowest_button = previous?;
            let restore_separator: NSRect = msg_send![ui.restore_defaults.separator, frame];
            Some(
                restore_separator.origin.y + restore_separator.size.height
                    <= lowest_button.origin.y,
            )
        })
        .unwrap_or(false);
        if !sidebar_layout_ok {
            log_info!(
                "[smoke-settings-layout] eight sidebar rows do not fit above the restore footer"
            );
            hide_settings();
            return false;
        }
        // The icon and its title must share one vertical mid-line. Its own check, with its own
        // message: folded into the geometry check above it would report a fit failure for an
        // alignment defect. Asserted from rendered pixels because the label's *frame* can be centred
        // while its ink is not -- which is the defect this replaced, and why a frame-only check
        // passed while the rows were visibly off.
        if !widgets::debug_validate_sidebar_ink_alignment() {
            log_info!(
                "[smoke-settings-layout] a sidebar icon and its title ink do not share a mid-line"
            );
            hide_settings();
            return false;
        }
        // Each sidebar button must carry its own page's title. The accessors key controls by page
        // identity; this is the live counter-example for a mismatched binding, because a wrong arm
        // would put one page's title (and its selection) on another page's button.
        let buttons_match_pages = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let buttons = sidebar_buttons(ui);
            let identities = settings_page_ids();
            Some(buttons.iter().zip(identities.iter()).all(|(button, page)| {
                widgets::sidebar_button_title(*button) == t(page.title_key())
            }))
        })
        .unwrap_or(false);
        if !buttons_match_pages {
            log_info!("[smoke-settings-layout] a sidebar button does not carry its page's title");
            hide_settings();
            return false;
        }
        // The rendered label must be a resolved string, never the key itself: `t()` returns the key
        // when no locale resolves it, so a wrong `title_key()` showed the key verbatim in the
        // sidebar. The check above compares the button against the same `t(page.title_key())` and is
        // self-consistent -- a raw key passes it -- so comparing against the key itself is the
        // counter-example that does not.
        let titles_are_resolved =
            with_settings_ui(|ui| {
                let ui = ui.as_ref()?;
                let buttons = sidebar_buttons(ui);
                let identities = settings_page_ids();
                Some(buttons.iter().zip(identities.iter()).all(|(button, page)| {
                    widgets::sidebar_button_title(*button) != page.title_key()
                }))
            })
            .unwrap_or(false);
        if !titles_are_resolved {
            log_info!("[smoke-settings-layout] a sidebar button rendered its raw translation key");
            hide_settings();
            return false;
        }
        // A panel-material change reflows the glass-style row and re-reads the appearance controls in
        // place; it must not rebuild the settings content (that rebuild used to freeze the main thread
        // on the click path). Drive the real config-apply path, assert the rebuild counter and the row's
        // effect on the General document, then restore the material so no test choice persists.
        {
            let old = crate::config::CONFIG.read().unwrap().clone();
            let target = if old.appearance.panel_material == "liquid-glass" {
                "frost"
            } else {
                "liquid-glass"
            };
            let general_document_height = || {
                with_settings_ui(|ui| {
                    ui.as_ref()
                        .map(|ui| {
                            let document: *mut AnyObject =
                                msg_send![ui.page_view(SettingsPageId::General), documentView];
                            let frame: NSRect = msg_send![document, frame];
                            frame.size.height
                        })
                        .unwrap_or(0.0)
                })
            };
            let before_height = general_document_height();
            let before_rebuilds = crate::settings::settings_content_rebuild_count();
            let mut next = old.clone();
            next.appearance.panel_material = target.into();
            *crate::config::CONFIG.write().unwrap() = next.clone();
            crate::runtime_config::apply_config_change(
                &old,
                &next,
                crate::runtime_config::ConfigChangeSource::Settings,
            );
            let after_rebuilds = crate::settings::settings_content_rebuild_count();
            let after_height = general_document_height();
            let applied_material_idx = with_settings_ui(|ui| {
                ui.as_ref()
                    .map(|u| {
                        let idx: isize = msg_send![u.panel_material, indexOfSelectedItem];
                        idx
                    })
                    .unwrap_or(-1)
            });
            // Restore first, so no failure path can leave the smoke's material behind.
            *crate::config::CONFIG.write().unwrap() = old.clone();
            crate::runtime_config::apply_config_change(
                &next,
                &old,
                crate::runtime_config::ConfigChangeSource::Settings,
            );
            let restored_height = general_document_height();
            let want_material =
                crate::config::panel_material_index_of(target).unwrap_or(0) as isize;
            if after_rebuilds != before_rebuilds || applied_material_idx != want_material {
                log_info!(
                    "[smoke-settings-layout] a panel-material change did not apply in place: rebuilds {before_rebuilds}->{after_rebuilds}, material {applied_material_idx} (want {want_material})"
                );
                hide_settings();
                return false;
            }
            // The row is the only structural difference between the two materials, so the document
            // height must change when the row toggles (and come back when the material is restored).
            if (after_height - before_height).abs() <= 0.5 {
                log_info!(
                    "[smoke-settings-layout] the glass-style row did not follow the material (before {before_height}, after {after_height}, target {target})"
                );
                hide_settings();
                return false;
            }
            if (restored_height - before_height).abs() > 0.5 {
                log_info!(
                    "[smoke-settings-layout] restoring the material did not restore the General document height ({before_height} -> {restored_height})"
                );
                hide_settings();
                return false;
            }
        }
        let selected_appearance_indices = || {
            with_settings_ui(|ui| {
                ui.as_ref()
                    .map(|u| {
                        let material: isize = msg_send![u.panel_material, indexOfSelectedItem];
                        let log: isize = msg_send![u.log_level, indexOfSelectedItem];
                        (material, log)
                    })
                    .unwrap_or((-1, -1))
            })
        };
        // Mixed import: a material + log-level change must keep both dropdowns in sync without
        // rebuilding. The log level has no narrow sync, so it is only right if the replay refilled
        // the controls in place. The real replay entry is driven; nothing is persisted.
        {
            let old = crate::config::CONFIG.read().unwrap().clone();
            let target_material = if old.appearance.panel_material == "liquid-glass" {
                "frost"
            } else {
                "liquid-glass"
            };
            let target_level = if old.logging.level == "debug" {
                "info"
            } else {
                "debug"
            };
            let mut next = old.clone();
            next.appearance.panel_material = target_material.into();
            next.logging.level = target_level.into();
            let before_rebuilds = crate::settings::settings_content_rebuild_count();
            *crate::config::CONFIG.write().unwrap() = next.clone();
            crate::settings::dispatch::apply_imported_runtime(&old, &next);
            let after_rebuilds = crate::settings::settings_content_rebuild_count();
            let (material_idx, log_idx) = selected_appearance_indices();
            // Restore first, so no failure path leaves the imported config behind.
            *crate::config::CONFIG.write().unwrap() = old.clone();
            crate::settings::dispatch::apply_imported_runtime(&next, &old);
            let want_material =
                crate::config::panel_material_index_of(target_material).unwrap_or(0) as isize;
            let want_log = if target_level == "debug" { 0 } else { 1 };
            if after_rebuilds != before_rebuilds
                || material_idx != want_material
                || log_idx != want_log
            {
                log_info!(
                    "[smoke-settings-layout] mixed import did not sync in place: rebuilds {before_rebuilds}->{after_rebuilds}, material {material_idx} (want {want_material}), log {log_idx} (want {want_log})"
                );
                hide_settings();
                return false;
            }
        }
        // General restore-defaults with the same mix, through the real restore entry. That entry
        // persists the restored config, so the caller's config is written back and persisted right
        // after the readback, before any assertion can bail out. Keeping theme, glass style, corner
        // radius and locale already at their defaults makes this exercise the in-place refill branch
        // instead of the rebuild one.
        {
            let saved = crate::config::CONFIG.read().unwrap().clone();
            let defaults = crate::config::Config::default();
            let mut skewed = saved.clone();
            skewed.appearance.theme = defaults.appearance.theme.clone();
            skewed.appearance.glass_style = defaults.appearance.glass_style.clone();
            skewed.appearance.corner_radius = defaults.appearance.corner_radius;
            skewed.i18n.locale = defaults.i18n.locale.clone();
            skewed.appearance.panel_material =
                if defaults.appearance.panel_material == "liquid-glass" {
                    "frost".into()
                } else {
                    "liquid-glass".into()
                };
            skewed.logging.level = if defaults.logging.level == "debug" {
                "info".into()
            } else {
                "debug".into()
            };
            *crate::config::CONFIG.write().unwrap() = skewed.clone();
            // Show the skewed values in the controls before the restore. Without this, a caller
            // whose config already holds the defaults would start from the expected state and the
            // checks below could pass with no refill at all.
            load_settings_values();
            let (skewed_material, skewed_log) = selected_appearance_indices();
            let want_material =
                crate::config::panel_material_index_of(&defaults.appearance.panel_material)
                    .unwrap_or(0) as isize;
            let want_log = if defaults.logging.level == "debug" {
                0
            } else {
                1
            };
            if skewed_material == want_material || skewed_log == want_log {
                log_info!(
                    "[smoke-settings-layout] the restore case did not establish non-default controls (material {skewed_material}, log {skewed_log})"
                );
                *crate::config::CONFIG.write().unwrap() = saved.clone();
                let _ = crate::config::flush_config_sync();
                hide_settings();
                return false;
            }
            let before_rebuilds = crate::settings::settings_content_rebuild_count();
            crate::settings::restore::restore_page_defaults(SettingsPageId::General);
            let after_rebuilds = crate::settings::settings_content_rebuild_count();
            let (material_idx, log_idx) = selected_appearance_indices();
            // Put the caller's config back on disk, synchronously, before any assertion can return:
            // the restore entry only queued a write of the defaults, and exiting without waiting
            // would leave them there.
            *crate::config::CONFIG.write().unwrap() = saved.clone();
            if let Err(error) = crate::config::flush_config_sync() {
                log_info!(
                    "[smoke-settings-layout] could not restore the caller's config to disk: {error}"
                );
                hide_settings();
                return false;
            }
            if after_rebuilds != before_rebuilds
                || material_idx != want_material
                || log_idx != want_log
            {
                log_info!(
                    "[smoke-settings-layout] General restore did not sync in place: rebuilds {before_rebuilds}->{after_rebuilds}, material {material_idx} (want {want_material}), log {log_idx} (want {want_log})"
                );
                hide_settings();
                return false;
            }
        }
        // Switching pages re-applies every sidebar title, but only the accent colour moves: the
        // titles, fonts and row height are unchanged, so the resolved geometry still holds and the
        // bitmap measurement must not run again. That measurement renders the button and scans it on
        // the main thread, so seven of them per page switch is the difference between an instant tab
        // and a visible stall.
        let before_switch = widgets::sidebar_ink_measurement_count();
        select_sidebar(if SIDEBAR_SELECTED.load(Ordering::SeqCst) == 0 {
            1
        } else {
            0
        });
        let after_switch = widgets::sidebar_ink_measurement_count();
        if after_switch != before_switch {
            log_info!(
                "[smoke-settings-layout] switching pages re-measured the sidebar ink {} time(s); a colour-only change must reuse the resolved geometry",
                after_switch - before_switch
            );
            hide_settings();
            return false;
        }
        let clipboard_shortcut_layout_ok = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            if ui.clipboard_shortcut.is_null()
                || ui.clipboard_shortcut_error.is_null()
                || ui.clipboard_pin_follow.is_null()
            {
                return Some(false);
            }
            let button_frame: NSRect = msg_send![ui.clipboard_shortcut, frame];
            let pin_frame: NSRect = msg_send![ui.clipboard_pin_follow, frame];
            let error_hidden: bool = msg_send![ui.clipboard_shortcut_error, isHidden];
            let button_is_native: bool =
                msg_send![ui.clipboard_shortcut, isKindOfClass: class!(NSButton)];
            let button_center = button_frame.origin.y + button_frame.size.height / 2.0;
            let button_label = SettingsRow::label_for(ui.clipboard_shortcut)?;
            let label_frame: NSRect = msg_send![button_label, frame];
            let label_center = label_frame.origin.y + label_frame.size.height / 2.0;
            let controls_center_gap = ((button_frame.origin.y + button_frame.size.height / 2.0)
                - (pin_frame.origin.y + pin_frame.size.height / 2.0))
                .abs();
            let expected_center_gap =
                (ui.clipboard_shortcut_row_height + ui.clipboard_pin_follow_row_height) / 2.0
                    + ui.clipboard_row_gap;
            let default_layout_ok = button_is_native
                && (button_frame.size.width - ROW_ACTION_BTN_W).abs() <= 0.5
                && (button_frame.size.height - ROW_ACTION_BTN_H).abs() <= 0.5
                && error_hidden
                && (button_center - label_center).abs() <= 1.0
                && (controls_center_gap - expected_center_gap).abs() <= 1.0;

            let current_shortcut = crate::config::CONFIG
                .read()
                .unwrap()
                .clipboard
                .shortcut
                .clone();
            dispatch::set_clipboard_shortcut_capture_state(
                ui,
                &current_shortcut,
                false,
                Some("layout smoke"),
            );
            let error_button_frame: NSRect = msg_send![ui.clipboard_shortcut, frame];
            let error_frame: NSRect = msg_send![ui.clipboard_shortcut_error, frame];
            let error_hidden: bool = msg_send![ui.clipboard_shortcut_error, isHidden];
            let error_layout_ok = !error_hidden
                && error_button_frame.origin.y >= error_frame.origin.y + error_frame.size.height;
            dispatch::set_clipboard_shortcut_capture_state(ui, &current_shortcut, false, None);

            Some(default_layout_ok && error_layout_ok)
        })
        .unwrap_or(false);
        if !clipboard_shortcut_layout_ok {
            log_info!(
                "[smoke-settings-layout] clipboard shortcut control has a blank row or invalid capture layout"
            );
            hide_settings();
            return false;
        }
        let _: () = msg_send![window, layoutIfNeeded];
        let opaque: bool = msg_send![window, isOpaque];
        if opaque {
            log_info!("[smoke-settings-layout] settings window unexpectedly opaque");
            hide_settings();
            return false;
        }
        let host: *mut AnyObject = msg_send![window, contentView];
        let root = settings_root_view_for_host(host);
        let root_layer: *mut AnyObject = if root.is_null() {
            std::ptr::null_mut()
        } else {
            msg_send![root, layer]
        };
        let root_clips: bool = if root_layer.is_null() {
            false
        } else {
            msg_send![root_layer, masksToBounds]
        };
        if !root_clips {
            log_info!("[smoke-settings-layout] settings root is not clipping rounded corners");
            hide_settings();
            return false;
        }
        let frame_before: NSRect = msg_send![window, frame];
        let radius_before: f64 = if root_layer.is_null() {
            0.0
        } else {
            msg_send![root_layer, cornerRadius]
        };
        let mut resized_frame = frame_before;
        resized_frame.size.height += 48.0;
        let _: () = msg_send![window, setFrame: resized_frame, display: true];
        let _: () = msg_send![window, layoutIfNeeded];
        refresh_settings_root_corner(window);
        let radius_after: f64 = if root_layer.is_null() {
            0.0
        } else {
            msg_send![root_layer, cornerRadius]
        };
        let radius_stable = radius_before.is_finite()
            && radius_after.is_finite()
            && radius_before >= 0.0
            && radius_after >= 0.0;
        if !radius_stable {
            log_info!("[smoke-settings-layout] root corner radius became invalid after resize");
            hide_settings();
            return false;
        }
        let children: *mut AnyObject = msg_send![root, subviews];
        let child_count: usize = msg_send![children, count];
        let mut solid_sidebar_ok = false;
        for index in 0..child_count {
            let child: *mut AnyObject = msg_send![children, objectAtIndex: index as isize];
            let identifier: *mut AnyObject = msg_send![child, identifier];
            if crate::ffi::nsstring_to_rust(identifier) != sidebar::SOLID_SIDEBAR_IDENTIFIER {
                continue;
            }
            let is_glass = AnyClass::get(c"NSGlassEffectView")
                .is_some_and(|class| msg_send![child, isKindOfClass: class]);
            let is_visual_effect = AnyClass::get(c"NSVisualEffectView")
                .is_some_and(|class| msg_send![child, isKindOfClass: class]);
            let layer: *mut AnyObject = msg_send![child, layer];
            let background = if layer.is_null() {
                std::ptr::null_mut()
            } else {
                crate::ffi::layer_background_color(layer)
            };
            let opaque: bool = if layer.is_null() {
                false
            } else {
                msg_send![layer, isOpaque]
            };
            let color_is_opaque =
                !background.is_null() && crate::ffi::CGColorGetAlpha(background) >= 0.999;
            solid_sidebar_ok = !is_glass && !is_visual_effect && opaque && color_is_opaque;
            break;
        }
        if !solid_sidebar_ok {
            log_info!("[smoke-settings-layout] sidebar is not using an opaque solid surface");
            hide_settings();
            return false;
        }
        for (index, page) in pages.iter().enumerate() {
            select_sidebar(index);
            let _: () = msg_send![window, layoutIfNeeded];
            scroll_page_to_top(*page);
            debug_validate_settings_page(*page, settings_page_name(index));
        }
        // The mouse page is filled lazily, *after* the sidebar has shown it, so its switches land on
        // a page the user is already looking at: the fill must apply the values without replaying the
        // switch spring. Force the master switch to the opposite value quietly, mark the page dirty,
        // let select_sidebar's fill restore it, and assert nothing animated.
        {
            let forced = with_settings_ui(|ui| {
                ui.as_ref().map(|ui| {
                    let enabled = crate::config::CONFIG.read().unwrap().mouse.enabled;
                    widgets::set_switch_state_immediate(ui.enable_mouse, !enabled);
                    (ui.enable_mouse, enabled)
                })
            });
            let Some((master, enabled)) = forced else {
                hide_settings();
                return false;
            };
            crate::settings::note_mouse_page_dirty();
            select_sidebar(SETTINGS_MOUSE_PAGE_INDEX);
            let state: isize = msg_send![master, state];
            let expected: isize = if enabled { 1 } else { 0 };
            let animating = switch_is_animating(master);
            if animating || state != expected {
                log_info!(
                    "[smoke-settings-layout] the mouse page fill animated its master switch (state={state}, expected={expected}, animating={animating})"
                );
                hide_settings();
                return false;
            }
        }
        // The About page's keychain row reads its state from the clipboard session and must follow it
        // WITHOUT a page switch or an app activation (a keychain grant can be silent, and a load can
        // land while the window is open). This is a gate rather than a re-check: the row is scribbled
        // over first and nothing here refreshes it by hand, so it can only pass if the production
        // path repainted it.
        {
            select_sidebar(SETTINGS_ABOUT_PAGE_INDEX);
            let scribbled = with_settings_ui(|ui| {
                let Some(ui) = ui.as_ref() else {
                    return false;
                };
                set_field(ui.keychain_access_status, "NOT-A-STATUS");
                true
            });
            assert!(scribbled, "the About page's keychain row must exist");
            // The public action the row's button performs (a full load here: the feature is on). The
            // work it starts lands on the main thread through `performSelectorOnMainThread`, which only
            // runs while this smoke pumps the run loop -- without this the assertion below would be
            // testing "the notification was queued", not "the row was repainted".
            let refreshes_before = keychain_row_refresh_count();
            crate::clipboard::retry_keychain_access();
            pump_run_loop_for(1.5);
            let refreshes_after = keychain_row_refresh_count();
            let (status_text, button_hidden, granted) = with_settings_ui(|ui| {
                let Some(ui) = ui.as_ref() else {
                    return (String::new(), true, false);
                };
                {
                    let status: *mut AnyObject = msg_send![ui.keychain_access_status, stringValue];
                    let hidden: bool = msg_send![ui.keychain_access_button, isHidden];
                    (
                        crate::ffi::nsstring_to_rust(status),
                        hidden,
                        crate::clipboard::storage_key_available(),
                    )
                }
            });
            let expected = if granted {
                t("settings.keychain_status_granted")
            } else {
                t("settings.keychain_status_missing")
            };
            if refreshes_after <= refreshes_before {
                log_info!(
                    "[smoke-settings-layout] the action did not repaint the keychain row (refreshes {refreshes_before}->{refreshes_after})"
                );
                hide_settings();
                return false;
            }
            if status_text != expected || button_hidden != granted {
                log_info!(
                    "[smoke-settings-layout] keychain row content wrong after the action: status {status_text:?} (expected {expected:?}), button_hidden {button_hidden}, granted {granted}"
                );
                hide_settings();
                return false;
            }
        }
        // The binding list's footer is one joined control whose two segments have different
        // availability (`＋` always while enabled, `−` only with a selection). Driving the production
        // enable pass and then asserting both flags is what catches a missing per-segment call.
        select_sidebar(SETTINGS_KEY_BINDINGS_PAGE_INDEX);
        let _: () = msg_send![window, layoutIfNeeded];
        // Drive the SAME function the master switch drives, so this checks the production path: a
        // helper that only inspected the current state would read whatever the last repaint happened
        // to leave. Both the row actions and the footer button have to follow the switch, so the two
        // checks run against one enable pass.
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                bindings::update_binding_controls_enabled(ui);
            }
        });
        if !bindings::debug_validate_row_actions_follow_master_switch() {
            hide_settings();
            return false;
        }
        if !bindings::debug_validate_footer_button_gating() {
            hide_settings();
            return false;
        }
        log_info!(
            "[smoke-settings-layout] row actions and the add button follow the master switch"
        );
        log_info!("[smoke-settings-layout] all pages passed");
        // Inline update content (release notes) expands the About page's Updates card. The page
        // document must grow with the card, otherwise the bottom of the notes is cut off and cannot
        // be scrolled to; the shared validator checks the expanded page against its document.
        // Inline update flow: the About page's Updates card grows *below* the check-button row, i.e.
        // below this page's document origin, so the document must grow with it (otherwise the release
        // notes below the origin are outside the scroll range) and the whole page must travel as one
        // block. Two cycles are checked: a normal one, and one after enlarging the window (it is
        // user-resizable). The window clamps at the display height, so a viewport taller than the
        // compact page -- where the fit is limited by the viewport instead of the content -- is not
        // reachable here; that arithmetic is covered by
        // `settings::tests::inline_update_room_follows_the_real_document_change`.
        select_sidebar(SETTINGS_ABOUT_PAGE_INDEX);
        let compact_state = update_flow_state();
        let compact = about_panel_snapshot();
        crate::settings::expand_update_section(UPDATE_CARD_EXPANSION_REQUEST);
        let _: () = msg_send![window, layoutIfNeeded];
        // The sidebar just scrolled About to its top and the expansion grew the document under it,
        // so reflow must follow the new top. Nothing re-tops the page here, so this fails when the
        // clip is left "growth" below the top -- the mouse page's materialize-after-scroll defect,
        // on a page this smoke can grow without steering.
        if !page_is_at_top(pages[SETTINGS_ABOUT_PAGE_INDEX]) {
            log_info!("[smoke-settings-layout] the expanded About page is not scrolled to its top");
            hide_settings();
            return false;
        }
        let expanded_state = update_flow_state();
        let expanded = about_panel_snapshot();
        let first_inside = update_card_inside_document();
        debug_validate_settings_page(pages[SETTINGS_ABOUT_PAGE_INDEX], "about-expanded");
        crate::settings::collapse_update_section();
        let _: () = msg_send![window, layoutIfNeeded];
        let collapsed_state = update_flow_state();
        let collapsed = about_panel_snapshot();
        let frame: NSRect = msg_send![window, frame];
        let taller = NSRect::new(
            frame.origin,
            NSSize::new(frame.size.width, frame.size.height + 220.0),
        );
        let _: () = msg_send![window, setFrame: taller, display: true];
        let _: () = msg_send![window, layoutIfNeeded];
        let tall_compact = about_panel_snapshot();
        crate::settings::expand_update_section(UPDATE_CARD_EXPANSION_REQUEST);
        let _: () = msg_send![window, layoutIfNeeded];
        let tall_expanded = about_panel_snapshot();
        let second_inside = update_card_inside_document();
        crate::settings::collapse_update_section();
        let _: () = msg_send![window, layoutIfNeeded];
        let tall_collapsed = about_panel_snapshot();

        // Reopening Settings hands every page's layout owner the current viewport
        // (`tighten_page_documents` -> `set_viewport`); the running flow's page must come back
        // unchanged, and the collapse must still land on the compact page.
        let offsets =
            with_settings_ui(|ui| ui.as_ref().map(|ui| capture_settings_scroll_offsets(ui)))
                .unwrap_or([NSPoint::new(0.0, 0.0); SETTINGS_PAGE_COUNT]);
        crate::settings::expand_update_section(UPDATE_CARD_EXPANSION_REQUEST);
        let _: () = msg_send![window, layoutIfNeeded];
        let reopened_before = about_panel_snapshot();
        show_settings_preserving(frame, SETTINGS_ABOUT_PAGE_INDEX, offsets);
        let reopened = about_panel_snapshot();
        let third_inside = update_card_inside_document();
        crate::settings::collapse_update_section();
        let _: () = msg_send![window, layoutIfNeeded];
        let reopened_collapsed = about_panel_snapshot();

        let (
            Some(compact),
            Some(expanded),
            Some(collapsed),
            Some(tall_compact),
            Some(tall_expanded),
            Some(tall_collapsed),
            Some(reopened_before),
            Some(reopened),
            Some(reopened_collapsed),
            Some(compact_state),
            Some(expanded_state),
            Some(collapsed_state),
        ) = (
            compact,
            expanded,
            collapsed,
            tall_compact,
            tall_expanded,
            tall_collapsed,
            reopened_before,
            reopened,
            reopened_collapsed,
            compact_state,
            expanded_state,
            collapsed_state,
        )
        else {
            log_info!("[smoke-settings-layout] inline update snapshots unavailable");
            hide_settings();
            return false;
        };
        // The page layout owner grows only the card and the space below it, so the checks are about
        // the document, the card and the views inside -- not about the whole page travelling.
        let inline_checks: [(&str, bool); 15] = [
            // The notification delegate's selector, sent with all three arguments (see
            // `will_present_smoke_check`): an arity slip crashes the app on the first real notice.
            (
                "notification_delegate_invokes_the_handler",
                crate::update_notice::will_present_smoke_check(),
            ),
            ("inside", first_inside && second_inside && third_inside),
            (
                "expand_grows",
                expanded.document_height > compact.document_height + MIN_UPDATE_DOCUMENT_GROWTH,
            ),
            ("collapse_restored", page_restored(&compact, &collapsed)),
            (
                "tall_expand_grows",
                tall_expanded.document_height
                    > tall_compact.document_height + MIN_UPDATE_DOCUMENT_GROWTH,
            ),
            (
                "tall_collapse_restored",
                page_restored(&tall_compact, &tall_collapsed),
            ),
            // The running flow's page has to survive the re-open untouched.
            (
                "reopen_stable",
                page_restored(&reopened_before, &reopened)
                    && page_restored(&compact, &reopened_collapsed),
            ),
            // Collapsing must land on the compact document exactly: the host row's consumption
            // returns to zero, not to a 1pt floor.
            (
                "collapse_exact",
                (collapsed_state.document_height - compact_state.document_height).abs() <= 0.5,
            ),
            // The card follows the host's bottom edge with the shared card inset (8pt), exactly
            // like every other card follows its last row.
            (
                "card_hugs_host",
                (expanded_state.card_rect_bottom_from_host_bottom + 8.0).abs() <= 0.5
                    && (collapsed_state.card_rect_bottom_from_host_bottom + 8.0).abs() <= 0.5,
            ),
            // The Update divider belongs on the shared row boundary. Compare its distance from the
            // centered switch to the switch's half-row inset, derived from the live control frame and
            // the row height; changing the mock's row height must not leave a stale pixel constant.
            (
                "divider_glued_to_row_above",
                [&compact_state, &expanded_state, &collapsed_state]
                    .iter()
                    .all(|state| {
                        let expected = -(components::SettingsLayout::SINGLE_LINE_ROW_H
                            - state.row_control_height)
                            / 2.0;
                        (state.divider_from_row_above - expected).abs() <= 0.5
                    }),
            ),
            // Visibility belongs to the owners: the collapsed page shows the check button and hides
            // the update host, the running flow replaces the button with the host, and collapsing
            // restores exactly that (a re-flow must never reveal a view its owner hid).
            (
                "visibility_compact",
                !compact_state.button_hidden && compact_state.host_hidden,
            ),
            (
                "visibility_expanded",
                expanded_state.button_hidden && !expanded_state.host_hidden,
            ),
            (
                "visibility_collapsed",
                !collapsed_state.button_hidden && collapsed_state.host_hidden,
            ),
            // The restore control's surface is a document sibling of its container: every re-flow has
            // to carry it along, and the control's own collapsed state must survive the flow.
            (
                "restore_surface_glued",
                (compact_state.restore_surface_from_container
                    - expanded_state.restore_surface_from_container)
                    .abs()
                    <= 0.5
                    && (compact_state.restore_surface_from_container
                        - collapsed_state.restore_surface_from_container)
                        .abs()
                        <= 0.5,
            ),
            (
                "restore_state_stable",
                !compact_state.restore_expanded
                    && !expanded_state.restore_expanded
                    && !collapsed_state.restore_expanded,
            ),
        ];
        let failed: Vec<&str> = inline_checks
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(name, _)| *name)
            .collect();
        if !failed.is_empty() {
            log_info!(
                "[smoke-settings-layout] inline update layout failed: {:?} (docs {:.1} {:.1} {:.1}) (flow {compact_state:?} {expanded_state:?} {collapsed_state:?})",
                failed,
                compact.document_height,
                expanded.document_height,
                collapsed.document_height,
            );
            hide_settings();
            return false;
        }
        log_info!("[smoke-settings-layout] update flow passed");
        // The sidebar search: a query from this locale's own copy has to find a row on any page,
        // and the results (here: the empty state) have to cover the page area exactly.
        if !sidebar::settings_search_smoke_check() {
            log_info!("[smoke-settings-layout] sidebar search layout failed");
            hide_settings();
            return false;
        }
        hide_settings();
        true
    }
}

/// Rebuild settings content and verify switcher and Keystroke Display values survive the rebuild.
pub(crate) fn settings_state_sync_smoke_runner() -> bool {
    unsafe {
        let mut cfg = CONFIG.read().unwrap().clone();
        cfg.windows.enabled = true;
        cfg.windows.show_minimized = true;
        cfg.windows.show_hidden_app_windows = true;
        cfg.windows.panel_close_enabled = true;
        cfg.windows.panel_minimize_enabled = true;
        cfg.windows.panel_hide_enabled = true;
        cfg.windows.panel_quit_enabled = true;
        cfg.layout.thumbnails_enabled = true;
        cfg.layout.focused_thumbnail_prewarm = true;
        cfg.layout.show_app_name_in_cards = true;
        cfg.keystroke_display.enabled = true;
        cfg.keystroke_display.mode = "commands".into();
        cfg.keystroke_display.tap_level = "hid".into();
        cfg.keystroke_display.display_position = "caret".into();
        cfg.keystroke_display.initial_position = "bottom".into();
        cfg.clipboard.separate_clear_all = true;
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg.clone();
        }

        show_settings();
        rebuild_settings_content_now();
        select_sidebar(SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX);

        let states = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let keystroke_page_document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::KeystrokeDisplay), documentView];
            let controls_on_keystroke_page = [
                ui.keystroke_display_enabled,
                ui.keystroke_display_mode,
                ui.keystroke_display_tap_level,
                ui.keystroke_display_position,
                ui.keystroke_display_initial_position,
            ]
            .into_iter()
            .all(|control| {
                if control.is_null() {
                    return false;
                }
                let parent: *mut AnyObject = msg_send![control, superview];
                parent == keystroke_page_document
            });
            // The window-related controls are grouped: Rust implements PartialEq/Debug for tuples
            // only up to 12 elements, and the flat form reached 16.
            let windows_controls = (
                msg_send![ui.windows_enabled, state],
                msg_send![ui.show_minimized, state],
                msg_send![ui.show_hidden_app_windows, state],
                msg_send![ui.panel_close_enabled, state],
                msg_send![ui.panel_minimize_enabled, state],
                msg_send![ui.panel_hide_enabled, state],
                msg_send![ui.panel_quit_enabled, state],
            );
            Some((
                windows_controls,
                msg_send![ui.thumbnails_enabled, indexOfSelectedItem],
                msg_send![ui.focused_thumbnail_prewarm, state],
                msg_send![ui.show_app_name_in_cards, state],
                msg_send![ui.keystroke_display_enabled, state],
                msg_send![ui.keystroke_display_mode, indexOfSelectedItem],
                msg_send![ui.keystroke_display_tap_level, indexOfSelectedItem],
                msg_send![ui.keystroke_display_position, indexOfSelectedItem],
                msg_send![ui.keystroke_display_initial_position, indexOfSelectedItem],
                controls_on_keystroke_page,
            ))
        });

        let mut refreshed_cfg = cfg;
        refreshed_cfg.keystroke_display.enabled = false;
        refreshed_cfg.keystroke_display.mode = "shortcuts".into();
        refreshed_cfg.keystroke_display.tap_level = "session".into();
        refreshed_cfg.keystroke_display.display_position = "main".into();
        refreshed_cfg.keystroke_display.initial_position = "left".into();
        if let Ok(mut current) = CONFIG.write() {
            *current = refreshed_cfg.clone();
        }
        refresh_switcher_keystroke_and_mouse_controls_from_config();
        let refreshed = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            Some((
                msg_send![ui.keystroke_display_enabled, state],
                msg_send![ui.keystroke_display_mode, indexOfSelectedItem],
                msg_send![ui.keystroke_display_tap_level, indexOfSelectedItem],
                msg_send![ui.keystroke_display_position, indexOfSelectedItem],
                msg_send![ui.keystroke_display_initial_position, indexOfSelectedItem],
            ))
        });
        // The "separate clear all" switch must follow a config reload in both directions while
        // the window stays open: the reload path in apply_config_change re-reads the switch
        // (the picker header relayout rides the same flag), so an open window never shows a
        // stale value after a config-file edit or defaults restore.
        let clear_switch_after_reload = |before: &Config, after: Config| -> Option<isize> {
            if let Ok(mut current) = CONFIG.write() {
                *current = after.clone();
            }
            apply_config_change(before, &after, ConfigChangeSource::Reload);
            with_settings_ui(|ui| {
                ui.as_ref()
                    .map(|u| msg_send![u.clipboard_separate_clear_all, state])
            })
        };
        let mut reloaded_off = refreshed_cfg.clone();
        reloaded_off.clipboard.separate_clear_all = false;
        let switch_off = clear_switch_after_reload(&refreshed_cfg, reloaded_off.clone());
        let mut reloaded_on = reloaded_off.clone();
        reloaded_on.clipboard.separate_clear_all = true;
        let switch_on = clear_switch_after_reload(&reloaded_off, reloaded_on);
        hide_settings();

        let rebuilt_matches = states
            == Some((
                (1isize, 1isize, 1isize, 1isize, 1isize, 1isize, 1isize),
                1isize,
                1isize,
                1isize,
                1isize,
                2isize,
                1isize,
                1isize,
                1isize,
                true,
            ));
        let refreshed_matches = refreshed == Some((0isize, 1isize, 0isize, 0isize, 2isize));
        let switch_follows_reload = switch_off == Some(0) && switch_on == Some(1);
        if !rebuilt_matches {
            log_info!("[smoke-settings-state-sync] rebuilt control state mismatch: {states:?}");
        }
        if !refreshed_matches {
            log_info!(
                "[smoke-settings-state-sync] refreshed control state mismatch: {refreshed:?}"
            );
        }
        if !switch_follows_reload {
            log_info!(
                "[smoke-settings-state-sync] separate-clear-all switch mismatch: off={switch_off:?} on={switch_on:?}"
            );
        }
        rebuilt_matches && refreshed_matches && switch_follows_reload
    }
}

/// Exercise the scroll-mode popup and reverse switch through their real AppKit callbacks.
pub(crate) fn settings_mouse_profile_callback_smoke_runner() -> bool {
    unsafe {
        let original_cfg = CONFIG.read().unwrap().clone();
        let mut smoke_cfg = original_cfg.clone();
        // Isolate the selected-device resolution while keeping this in-memory smoke snapshot
        // independent of the user's mouse profiles and never persisted.
        smoke_cfg.mouse.enabled = true;
        smoke_cfg.mouse.profiles = vec![crate::config::MouseProfile {
            reverse_scroll: Some(false),
            scroll_mode: Some("line".into()),
            line_count: Some(7),
            smooth_scrolling: Some(crate::config::SmoothPartial {
                preset: Some("custom".into()),
                response: Some(1.37),
                speed: Some(2.3),
                acceleration: Some(3.4),
                inertia: Some(4.5),
                ..Default::default()
            }),
            ..Default::default()
        }];
        if let Ok(mut current) = CONFIG.write() {
            *current = smoke_cfg;
        }

        show_settings();
        select_sidebar(2);
        let controls = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            Some((
                ui.scroll_mode,
                ui.line_count,
                ui.smooth_scrolling_preset,
                ui.reverse_scroll,
            ))
        });

        let passed = (|| {
            let Some((mode_popup, line_count, smooth_preset, reverse_switch)) = controls else {
                return false;
            };

            let select_mode = |index: isize| {
                let _: () = msg_send![mode_popup, selectItemAtIndex: index];
                let target: *mut AnyObject = msg_send![mode_popup, target];
                let action: Sel = msg_send![mode_popup, action];
                let sent: bool = msg_send![mode_popup, sendAction: action, to: target];
                sent
            };

            let initially_line = with_settings_ui(|ui| {
                let ui = ui.as_ref()?;
                let mode: isize = msg_send![ui.scroll_mode, indexOfSelectedItem];
                let line_hidden: bool = msg_send![line_count, isHidden];
                let smooth_hidden: bool = msg_send![smooth_preset, isHidden];
                Some((mode == 1, !line_hidden, smooth_hidden))
            });
            let line_to_smooth_dispatched = select_mode(2);
            let smooth_state = with_settings_ui(|ui| {
                ui.as_ref()?;
                let line_hidden: bool = msg_send![line_count, isHidden];
                let line_enabled: bool = msg_send![line_count, isEnabled];
                let smooth_hidden: bool = msg_send![smooth_preset, isHidden];
                let smooth_enabled: bool = msg_send![smooth_preset, isEnabled];
                Some((line_hidden, line_enabled, smooth_hidden, smooth_enabled))
            });
            let smooth_config = {
                let cfg = CONFIG.read().unwrap();
                resolve_selected_from(&cfg)
            };

            let smooth_to_line_dispatched = select_mode(1);
            let line_state = with_settings_ui(|ui| {
                ui.as_ref()?;
                let line_hidden: bool = msg_send![line_count, isHidden];
                let line_enabled: bool = msg_send![line_count, isEnabled];
                let smooth_hidden: bool = msg_send![smooth_preset, isHidden];
                Some((line_hidden, line_enabled, smooth_hidden))
            });
            let line_config = {
                let cfg = CONFIG.read().unwrap();
                resolve_selected_from(&cfg)
            };

            let initial_reverse: isize = msg_send![reverse_switch, state];
            let _: () = msg_send![reverse_switch, performClick: std::ptr::null::<AnyObject>()];
            let toggled_reverse = {
                let cfg = CONFIG.read().unwrap();
                resolve_selected_from(&cfg).reverse_scroll
            };
            let toggled_reverse_state: isize = msg_send![reverse_switch, state];
            let _: () = msg_send![reverse_switch, performClick: std::ptr::null::<AnyObject>()];
            let restored_reverse = {
                let cfg = CONFIG.read().unwrap();
                resolve_selected_from(&cfg).reverse_scroll
            };

            initially_line == Some((true, true, true))
                && line_to_smooth_dispatched
                && smooth_state == Some((true, false, false, true))
                && smooth_config.scroll_mode == crate::mouse::scrolling::ScrollMode::Smooth
                && smooth_config.line_count == 7
                && (smooth_config.smooth_scrolling.response - 1.37).abs() < 1e-9
                && (smooth_config.smooth_scrolling.speed - 2.3).abs() < 1e-9
                && smooth_to_line_dispatched
                && line_state == Some((false, true, true))
                && line_config.scroll_mode == crate::mouse::scrolling::ScrollMode::Line
                && line_config.line_count == 7
                && (line_config.smooth_scrolling.response - 1.37).abs() < 1e-9
                && toggled_reverse == (toggled_reverse_state == 1)
                && toggled_reverse != (initial_reverse == 1)
                && !restored_reverse
        })();

        if let Ok(mut current) = CONFIG.write() {
            *current = original_cfg;
        }
        refresh_switcher_keystroke_and_mouse_controls_from_config();
        hide_settings();
        passed
    }
}

/// Verify that collapsing the clipboard child row leaves adjacent controls correctly laid out.
pub(crate) fn settings_collapsible_row_smoke_runner() -> bool {
    unsafe {
        let mut cfg = CONFIG.read().unwrap().clone();
        cfg.clipboard.enabled = true;
        cfg.clipboard.delete_after_paste = true;
        for profile in &mut cfg.mouse.profiles {
            profile.scroll_mode = Some("line".into());
            let pointer = profile.pointer.get_or_insert_with(Default::default);
            pointer.disable_acceleration = Some(true);
        }
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg;
        }

        show_settings();
        let before = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            let document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::Clipboard), documentView];
            let document_height: NSRect = msg_send![document, frame];
            Some((previous, next, auto_expire, document_height.size.height))
        });
        let Some((previous_before, next_before, auto_before, document_before)) = before else {
            hide_settings();
            return false;
        };

        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                let _: () = msg_send![ui.clipboard_delete_after_paste, setState: 1isize];
            }
        });
        let clipboard_switch =
            with_settings_ui(|ui| ui.as_ref().map(|ui| ui.clipboard_delete_after_paste));
        let Some(clipboard_switch) = clipboard_switch else {
            hide_settings();
            return false;
        };
        let _: () = msg_send![clipboard_switch, performClick: std::ptr::null::<AnyObject>()];
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                let _: () = msg_send![ui.window, layoutIfNeeded];
            }
        });
        let collapsed = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let child_hidden: bool =
                msg_send![ui.clipboard_clear_system_pasteboard_after_paste, isHidden];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            let document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::Clipboard), documentView];
            let document_height: NSRect = msg_send![document, frame];
            Some((
                previous,
                child_hidden,
                next,
                auto_expire,
                document_height.size.height,
            ))
        });
        let Some((previous_after, child_hidden, next_after, auto_after, document_after)) =
            collapsed
        else {
            hide_settings();
            return false;
        };

        let moved_by = next_after.origin.y - next_before.origin.y;
        let auto_moved_by = auto_after.origin.y - auto_before.origin.y;
        let expected_shift = SettingsLayout::new(400.0).row_gap + SettingsLayout::SINGLE_LINE_ROW_H;
        // The page layout owner anchors the page to the document's top edge, so hiding the row
        // shortens the document and everything above the group moves down by that amount in
        // document coordinates; the rows below keep their y (the gap still closes on screen).
        let document_shrank = document_before - document_after;
        let previous_moved_by = previous_after.origin.y - previous_before.origin.y;
        let stable_previous = previous_before.origin.x == previous_after.origin.x
            && previous_before.size.width == previous_after.size.width
            && previous_before.size.height == previous_after.size.height
            && (previous_moved_by + expected_shift).abs() < 1.0;
        let collapsed_ok = child_hidden
            && stable_previous
            && (document_shrank - expected_shift).abs() < 1.0
            && moved_by.abs() < 1.0
            && auto_moved_by.abs() < 1.0;
        if !collapsed_ok {
            eprintln!(
                "[smoke-settings-collapsible-row] hidden={child_hidden} previous_stable={stable_previous} previous_y={:.1}->{:.1} next_y={:.1}->{:.1} move={moved_by:.1} auto_move={auto_moved_by:.1} document={:.1}->{:.1} expected_shift={expected_shift:.1}",
                previous_before.origin.y,
                previous_after.origin.y,
                next_before.origin.y,
                next_after.origin.y,
                document_before,
                document_after,
            );
        }

        let fitted_after_collapse = tighten_page_documents();
        let after_fit = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            Some((previous, next, auto_expire))
        });
        let Some((previous_after_fit, next_after_fit, auto_after_fit)) = after_fit else {
            hide_settings();
            return false;
        };

        let _: () = msg_send![clipboard_switch, performClick: std::ptr::null::<AnyObject>()];
        let expanded = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            Some((previous, next, auto_expire))
        });
        // The Clipboard page owns its layout, so the page pass has nothing left to fit, and
        // expanding the row again restores the baseline exactly (document height and every row).
        let restored = expanded.is_some_and(|(previous, next, auto_expire)| {
            !fitted_after_collapse
                && (previous_after_fit.origin.y - previous_after.origin.y).abs() < 1.0
                && (next_after_fit.origin.y - next_after.origin.y).abs() < 1.0
                && (auto_after_fit.origin.y - auto_after.origin.y).abs() < 1.0
                && (previous.origin.y - previous_before.origin.y).abs() < 1.0
                && (next.origin.y - next_before.origin.y).abs() < 1.0
                && (auto_expire.origin.y - auto_before.origin.y).abs() < 1.0
        });
        if !restored {
            eprintln!(
                "[smoke-settings-collapsible-row] reopen after the row came back failed: fitted={fitted_after_collapse}"
            );
        }

        // The Mouse page owns two row groups in two cards. Collapsing both and reopening only the
        // upper one must leave the lower one hidden and size the document for exactly the rows that
        // are visible.
        select_sidebar(2);
        let mouse_before = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::Mouse), documentView];
            let document_frame: NSRect = msg_send![document, frame];
            Some((
                document_frame.size.height,
                ui.scroll_mode,
                ui.disable_pointer_accel,
            ))
        });
        let Some((mouse_document_full, _, _)) = mouse_before else {
            hide_settings();
            return false;
        };
        // Collapse both groups. They are driven through the layout owner directly: this guard covers
        // the geometry (which rows move, how far the document shrinks), while the switches' own
        // state sync belongs to the settings state-sync smoke.
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
                    .set_group_visible(RowGroup::LineCount, false);
                ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
                    .set_group_visible(RowGroup::PointerAccel, false);
            }
        });
        let mouse_collapsed = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::Mouse), documentView];
            let document_frame: NSRect = msg_send![document, frame];
            let line_hidden: bool = msg_send![ui.line_count, isHidden];
            let pointer_hidden: bool = msg_send![ui.pointer_accel_slider, isHidden];
            Some((document_frame.size.height, line_hidden, pointer_hidden))
        });
        // Reopen only the upper group.
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
                    .set_group_visible(RowGroup::LineCount, true);
            }
        });
        let mouse_after_upper = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::Mouse), documentView];
            let document_frame: NSRect = msg_send![document, frame];
            let line_hidden: bool = msg_send![ui.line_count, isHidden];
            let pointer_hidden: bool = msg_send![ui.pointer_accel_slider, isHidden];
            Some((document_frame.size.height, line_hidden, pointer_hidden))
        });
        let mouse_interleaving_ok = match (mouse_collapsed, mouse_after_upper) {
            (
                Some((collapsed_document, line_hidden, pointer_hidden)),
                Some((upper_document, line_visible_again, pointer_hidden_again)),
            ) => {
                let group = SettingsLayout::new(400.0).row_gap + SettingsLayout::SINGLE_LINE_ROW_H;
                (mouse_document_full - collapsed_document - 2.0 * group).abs() < 1.0
                    && line_hidden
                    && pointer_hidden
                    && (upper_document - (collapsed_document + group)).abs() < 1.0
                    && !line_visible_again
                    && pointer_hidden_again
            }
            _ => false,
        };
        // Bring the tracking-speed row back: both groups visible again, and the document back to
        // the height it started with.
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.page_canvases[SETTINGS_MOUSE_PAGE_INDEX]
                    .set_group_visible(RowGroup::PointerAccel, true);
            }
        });
        let mouse_document_restored = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject =
                msg_send![ui.page_view(SettingsPageId::Mouse), documentView];
            let document_frame: NSRect = msg_send![document, frame];
            let line_hidden: bool = msg_send![ui.line_count, isHidden];
            let pointer_hidden: bool = msg_send![ui.pointer_accel_slider, isHidden];
            Some(
                (document_frame.size.height - mouse_document_full).abs() < 1.0
                    && !line_hidden
                    && !pointer_hidden,
            )
        });
        if !mouse_interleaving_ok || mouse_document_restored != Some(true) {
            eprintln!(
                "[smoke-settings-collapsible-row] mouse interleaving failed: full={mouse_document_full:.1} collapsed={mouse_collapsed:?} upper={mouse_after_upper:?} restored={mouse_document_restored:?}"
            );
        }
        hide_settings();
        collapsed_ok && restored && mouse_interleaving_ok && mouse_document_restored == Some(true)
    }
}

/// Close this app's settings window from the switcher. This must run directly on the main thread,
/// rather than indirectly through a background AX action callback.
pub(crate) fn close_settings_from_switcher() {
    hide_settings();
}

/// Show a simple app-modal alert for validation / save errors.
pub(super) fn show_alert(title: &str, msg: &str) {
    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];
        let ns1 = make_nsstring(title);
        let _: () = msg_send![alert, setMessageText: ns1];
        CFRelease(ns1 as *const c_void);
        let ns2 = make_nsstring(msg);
        let _: () = msg_send![alert, setInformativeText: ns2];
        CFRelease(ns2 as *const c_void);
        let ns3 = make_nsstring(&t("alert.btn_ok"));
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: ns3];
        CFRelease(ns3 as *const c_void);
        let _resp: isize = msg_send![alert, runModal];
        release_obj(alert);
    }
}

mod chrome;

pub(crate) fn settings_vertical_zoom_smoke_runner() -> bool {
    unsafe {
        show_settings();
        let Some(window) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)) else {
            return false;
        };
        let screen: *mut AnyObject = msg_send![window, screen];
        if screen.is_null() {
            hide_settings();
            return false;
        }
        let visible: NSRect = msg_send![screen, visibleFrame];
        let mut initial: NSRect = msg_send![window, frame];
        // Start below the available height even on a small display, so the first click must grow.
        let min: NSSize = msg_send![window, minSize];
        initial.size.height = (visible.size.height * 0.6).max(min.height);
        initial.origin.y = visible.origin.y + (visible.size.height - initial.size.height) / 2.0;
        let _: () = msg_send![window, setFrame: initial, display: true];
        let initial: NSRect = msg_send![window, frame];
        let green = traffic_light_buttons(window)[2];
        if green.is_null() {
            hide_settings();
            return false;
        }
        let _: () = msg_send![green, performClick: std::ptr::null::<AnyObject>()];
        pump_run_loop_for(0.5);
        let zoomed: NSRect = msg_send![window, frame];
        let behavior: u64 = msg_send![window, collectionBehavior];
        let style: u64 = msg_send![window, styleMask];
        let expanded = (zoomed.origin.x - initial.origin.x).abs() < 0.5
            && (zoomed.size.width - initial.size.width).abs() < 0.5
            && (zoomed.origin.y - visible.origin.y).abs() < 0.5
            && (zoomed.size.height - visible.size.height).abs() < 0.5
            && behavior & SETTINGS_ZOOM_COLLECTION_BEHAVIOR == SETTINGS_ZOOM_COLLECTION_BEHAVIOR
            && style & (1 << 14) == 0; // NSWindowStyleMaskFullScreen
        let green = traffic_light_buttons(window)[2];
        if green.is_null() {
            hide_settings();
            return false;
        }
        let _: () = msg_send![green, performClick: std::ptr::null::<AnyObject>()];
        pump_run_loop_for(0.5);
        let restored: NSRect = msg_send![window, frame];
        let restored_ok = (restored.origin.x - initial.origin.x).abs() < 0.5
            && (restored.origin.y - initial.origin.y).abs() < 0.5
            && (restored.size.width - initial.size.width).abs() < 0.5
            && (restored.size.height - initial.size.height).abs() < 0.5;
        log_info!(
            "[smoke-settings-vertical-zoom] expanded={expanded} restored={restored_ok} initial={initial:?} zoomed={zoomed:?} visible={visible:?} final={restored:?}"
        );
        hide_settings();
        expanded && restored_ok
    }
}

#[cfg(test)]
pub(super) use chrome::settings_effective_corner_radius;
pub(super) use chrome::{
    apply_settings_root_surface, apply_settings_window_appearance, build_settings_titlebar_band,
    refresh_settings_root_corner, settings_root_view_class, settings_root_view_for_host,
    settings_titlebar_view_for_host, settings_window_class, settings_window_class_handle,
    traffic_light_buttons, SETTINGS_ZOOM_COLLECTION_BEHAVIOR, TITLEBAR_H,
};

fn create_settings_window() {
    create_settings_window_for(None);
}

/// The detail pane's page area inside the content view: from the title-bar band's bottom edge down
/// to the window's own bottom edge.
///
/// The pages are built into this frame, and the search results that stand in for them use the same
/// one, so the two can neither drift from the band nor leave a strip of unused pane below. The
/// content view is not flipped, so origin 0 is the window's bottom edge and the band is what the
/// height is measured against.
pub(super) fn settings_page_frame(
    page_x: f64,
    page_width: f64,
    band_h: f64,
    content_h: f64,
) -> NSRect {
    NSRect::new(
        NSPoint::new(page_x, 0.0),
        NSSize::new(page_width, (content_h - band_h).max(1.0)),
    )
}

/// Rebuilds the settings content (after a scroller-style change, to fit the new visible width).
pub(crate) unsafe fn rebuild_settings_content_now() {
    if let Some(window) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)) {
        // Same rule as the full rebuild: a floating editor holds a dim layer parented to the content
        // view this is about to replace, so it has to go first (the scroller-style change reaches
        // here directly).
        close_floating_editors();
        rebuild_settings_content(window);
        // Rebuilding creates fresh controls, so restore the current configuration afterward.
        load_settings_values();
    }
}

/// Rebuild the settings content in an existing window without replacing the window itself.
fn rebuild_settings_content(window: *mut AnyObject) {
    create_settings_window_for(Some(window));
}

fn create_settings_window_for(existing_window: Option<*mut AnyObject>) {
    // Resolve the control target BEFORE anything is torn down: this used to bail at the sidebar
    // build, after `note_settings_content_rebuilt` and the subview removal had already run, which
    // left a window with no content and a stale search index -- searches then matched nothing (or
    // worse, resolved stale rows) until a rebuild happened to succeed.
    let Some(target) = (*MENU_TARGET.lock().unwrap()).map(|t| t.0) else {
        return;
    };
    // Every control is new: whatever the mouse page held is gone with the old views.
    crate::settings::note_settings_content_rebuilt();
    unsafe {
        let palette = settings_palette();
        // Keep the original compact settings window dimensions while applying the redesign's
        // typography, spacing, controls, and grouped-card treatment.
        // Give the redesigned detail pane enough room for full labels, links, and wide fields
        // while keeping the sidebar and the existing window height unchanged.
        // The mock's `.settings-window` measures 815.39pt at full width; 816 is that width on the
        // 4pt grid. It also feeds `setMinSize`/`setMaxSize`, so the fixed width stays consistent.
        let view_w = 732.0;
        let card_margin = 0.0;
        let card_w = sidebar::SIDEBAR_WIDTH;
        // HTML `--radius-window: 18px`: the window's own corner follows the panel radius instead
        // of the toolbar style's measured 26pt frame radius.
        let window_clip_radius = crate::theme::RADIUS_PANEL;
        let card_radius = 0.0;
        let style: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
        // titled + closable + miniaturizable + resizable (all three traffic lights). resizable is
        // required for the native zoom button; width is fixed and height remains adjustable.
        // The page content uses generous spacing and grouped cards so the controls remain easy
        // to scan without compressing the taller sections.
        // Initial position: centered on the primary display (screens[0]). Don't use
        // NSScreen.mainScreen (it follows the key window, not the primary display; see
        // overlay_target_screen's note).
        let win_w = view_w;
        let win_h = 720.0;
        let window = if let Some(window) = existing_window {
            window
        } else {
            let (win_x, win_y) = {
                let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
                let count: usize = msg_send![screens, count];
                if count > 0 {
                    // objectAtIndex: expects 'q' (signed long); pass isize.
                    let s: *mut AnyObject = msg_send![screens, objectAtIndex: 0isize];
                    let f: NSRect = msg_send![s, frame];
                    (
                        f.origin.x + (f.size.width - win_w) / 2.0,
                        f.origin.y + (f.size.height - win_h) / 2.0,
                    )
                } else {
                    (220.0, 180.0)
                }
            };
            let frame = NSRect::new(NSPoint::new(win_x, win_y), NSSize::new(win_w, win_h));
            let window: *mut AnyObject = msg_send![settings_window_class(), alloc];
            msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false]
        };
        apply_settings_window_appearance(window);
        // Empty areas of the settings content should remain usable as a drag surface, like the
        // title-bar band. AppKit keeps controls and other interactive views in charge of their
        // own mouse events while moving the window from its background.
        let _: () = msg_send![window, setMovableByWindowBackground: true];
        let _: () = msg_send![window, setCollectionBehavior: SETTINGS_ZOOM_COLLECTION_BEHAVIOR];
        // NSWindow's delegate is weak. The window is its own delegate so the standard-frame
        // callback has exactly the same lifetime, without another retained object or a cycle.
        let _: () = msg_send![window, setDelegate: window];
        let ns_title = make_nsstring(&t("settings.window_title"));
        let _: () = msg_send![window, setTitle: ns_title];
        CFRelease(ns_title as *const c_void);
        if existing_window.is_none() {
            let _: () = msg_send![window, setReleasedWhenClosed: false];
            // Fixed width: min and max width both equal the designed width, height stays adjustable --
            // same as System Settings (the width cannot be dragged). The themed frame comes from the
            // app's own band and root surface, not from an NSToolbar.
            let _: () = msg_send![window, setMinSize: NSSize::new(view_w, 400.0)];
            let _: () = msg_send![window, setMaxSize: NSSize::new(view_w, 10000.0)];

            // No NSToolbar: a unified toolbar paints a glass material strip across the top of the
            // window. The mock's title bar band is an opaque, palette-colored strip instead
            // (build_settings_titlebar_band), so the toolbar would only double-paint under it.
            // Nothing else in the window needed it -- it carried no items and no responders.
        }

        let host: *mut AnyObject = msg_send![window, contentView];
        let content: *mut AnyObject = if existing_window.is_none() {
            let host_bounds: NSRect = msg_send![host, bounds];
            let root: *mut AnyObject = msg_send![settings_root_view_class(), alloc];
            let root: *mut AnyObject = msg_send![root, initWithFrame: host_bounds];
            let _: () = msg_send![root, setAutoresizingMask: 18u64];
            let _: () = msg_send![host, addSubview: root];
            // Keep one custom root under the system content host. It is the only layer that
            // paints the settings background and clips child surfaces to the window shape.
            release_obj(root);
            root
        } else {
            settings_root_view_for_host(host)
        };
        // The existing settings window should always retain the custom root. If AppKit replaced
        // the content host's children during a style transition, recreate it before rebuilding.
        let content: *mut AnyObject = if content.is_null() {
            let host_bounds: NSRect = msg_send![host, bounds];
            let root: *mut AnyObject = msg_send![settings_root_view_class(), alloc];
            let root: *mut AnyObject = msg_send![root, initWithFrame: host_bounds];
            let _: () = msg_send![root, setAutoresizingMask: 18u64];
            let _: () = msg_send![host, addSubview: root];
            release_obj(root);
            root
        } else {
            content
        };
        if existing_window.is_some() {
            // Remove only the old content hierarchy; keep the NSWindow, its frame, and key status
            // intact so a locale/theme refresh is an in-place redraw.
            //
            // The whole old hierarchy is about to be destroyed: clear the tooltip registries first
            // (their keys are view addresses). A stale address surviving into the next click is
            // usually reused by a new control -- i.e. a message to a freed object (exactly the
            // 2026-09-15 22:19 SIGTRAP crash path: settings_window_send_event -> tooltip hits a
            // stale address).
            // It also has to cover the registries the author did not think of here: the rows and
            // the selects are address-keyed too, and the sidebar is worse than a lookup table
            // because sidebar_button_under_pointer ITERATES SIDEBAR_TITLE_LABELS as the list of
            // buttons and messages each one, so a stale key traps in objc's receiver check on the
            // next hover (2026-10-01 17:47 SIGTRAP: NSTrackingArea ->
            // sidebar_hover_tracker_mouse_entered -> sidebar_button_under_pointer). Clearing all of
            // them before the hierarchy goes away -- not only when the sidebar is rebuilt -- means
            // nothing can observe a dangling view.
            clear_settings_content_registries();
            let subviews: *mut AnyObject = msg_send![content, subviews];
            let count: usize = msg_send![subviews, count];
            for index in (0..count).rev() {
                let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
                let _: () = msg_send![subview, removeFromSuperview];
            }
        }
        let content_frame: NSRect = msg_send![content, frame];
        let content_h = content_frame.size.height;

        // Flip the content into the title bar before measuring: the mock draws its own band
        // (.settings-titlebar, background var(--panel-solid), a 1pt --settings-divider under it)
        // instead of AppKit's chrome, so AppKit's title, toolbar hairline and glass toolbars stay out
        // of the window. AppKit retains ownership of the traffic lights and their layout.
        let _: () = msg_send![window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![window, setStyleMask: style | (1 << 15)]; // NSWindowStyleMaskFullSizeContentView
        let _: () = msg_send![window, setTitleVisibility: 1isize]; // NSWindowTitleHidden

        // The mock's title bar band (`.settings-titlebar`, 36px) is not chrome the content may paint
        // under: its layout starts BELOW the band, exactly like the mock's `.settings-layout` begins
        // under it. The full-height containers are therefore measured from the band's bottom edge, and
        // the band is painted once above them (see build_settings_titlebar_band). There is no bottom
        // footer bar anymore: each page embeds its own restore control at the end of its content.
        // The band is the mock's fixed 36pt (docs/design-style: "Settings title bar band | 36 tall,
        // full window width"), not AppKit's measured title-bar area -- that measured 32pt here and
        // left the page and the sidebar starting 4pt too high. The page and the sidebar both start
        // under the band, exactly where the mock's .settings-layout starts.
        let band_h = TITLEBAR_H;
        let layout_h = (content_h - band_h).max(1.0);
        let page_viewport_h = layout_h;

        // The root surface owns the window background and clipping. On macOS 27 it follows the
        // system's toolbar-window radius through container concentricity; older systems use the
        // measured fallback radius.
        apply_settings_root_surface(window, content, palette, window_clip_radius);

        // The mock's band spans the whole window: over the sidebar column as an opaque strip
        // (var(--panel-solid)) with a --settings-divider hairline under it, and over the detail pane
        // the root surface's window_bg already is that strip. The title is the same 12pt muted label
        // the mock centers there. Both are siblings of the sidebar, added before it.
        build_settings_titlebar_band(content, content_h, band_h, view_w, palette);
        // AppKit's own window title is hidden (setTitleVisibility:), so the band's centered label is
        // the only title the window draws.

        let mut ui = SettingsUi {
            permission_warning_label: std::ptr::null_mut(),
            permission_warning_button: std::ptr::null_mut(),
            page_viewport_h,
            window,
            settings_search: sidebar::SettingsSearch::EMPTY,
            sidebar_highlight: std::ptr::null_mut(),
            sidebar_buttons: [std::ptr::null_mut(); SETTINGS_PAGE_COUNT],
            page_views: [std::ptr::null_mut(); SETTINGS_PAGE_COUNT],
            about_subtitle: std::ptr::null_mut(),
            accessibility_permission_status: std::ptr::null_mut(),
            accessibility_permission_button: std::ptr::null_mut(),
            screen_recording_permission_status: std::ptr::null_mut(),
            keychain_access_status: std::ptr::null_mut(),
            keychain_access_button: std::ptr::null_mut(),
            theme: std::ptr::null_mut(),
            glass_style: std::ptr::null_mut(),
            panel_material: std::ptr::null_mut(),
            corner_radius: std::ptr::null_mut(),
            thumbnails_enabled: std::ptr::null_mut(),
            focused_thumbnail_prewarm: std::ptr::null_mut(),
            show_app_name_in_cards: std::ptr::null_mut(),
            thumbnail_size: std::ptr::null_mut(),
            card_text_size: std::ptr::null_mut(),
            card_text_size_value_label: std::ptr::null_mut(),
            status_bar_text_size: std::ptr::null_mut(),
            status_bar_text_size_value_label: std::ptr::null_mut(),
            show_footer_title: std::ptr::null_mut(),
            modifier: std::ptr::null_mut(),
            locale: std::ptr::null_mut(),
            panel_close_enabled: std::ptr::null_mut(),
            panel_minimize_enabled: std::ptr::null_mut(),
            panel_hide_enabled: std::ptr::null_mut(),
            panel_quit_enabled: std::ptr::null_mut(),
            show_minimized: std::ptr::null_mut(),
            show_hidden_app_windows: std::ptr::null_mut(),
            show_other_desktops: std::ptr::null_mut(),
            windows_enabled: std::ptr::null_mut(),
            overlay_position: std::ptr::null_mut(),
            activation_mode: std::ptr::null_mut(),
            keystroke_display_enabled: std::ptr::null_mut(),
            keystroke_display_mode: std::ptr::null_mut(),
            keystroke_display_tap_level: std::ptr::null_mut(),
            keystroke_display_position: std::ptr::null_mut(),
            keystroke_display_initial_position: std::ptr::null_mut(),
            keystroke_display_max_extent: std::ptr::null_mut(),
            keystroke_display_max_extent_value: std::ptr::null_mut(),
            log_level: std::ptr::null_mut(),
            launch_at_login: std::ptr::null_mut(),
            hide_menu_bar_icon: std::ptr::null_mut(),
            reverse_scroll: std::ptr::null_mut(),
            enable_mouse: std::ptr::null_mut(),
            scroll_mode: std::ptr::null_mut(),
            line_count: std::ptr::null_mut(),
            line_count_label: std::ptr::null_mut(),
            line_count_value_label: std::ptr::null_mut(),
            disable_pointer_accel: std::ptr::null_mut(),
            pointer_accel_slider: std::ptr::null_mut(),
            pointer_accel_label: std::ptr::null_mut(),
            pointer_accel_value_label: std::ptr::null_mut(),
            smooth_scrolling_preset: std::ptr::null_mut(),
            smooth_scrolling_response: std::ptr::null_mut(),
            smooth_scrolling_response_value: std::ptr::null_mut(),
            smooth_scrolling_speed: std::ptr::null_mut(),
            smooth_scrolling_speed_value: std::ptr::null_mut(),
            smooth_scrolling_acceleration: std::ptr::null_mut(),
            smooth_scrolling_acceleration_value: std::ptr::null_mut(),
            smooth_scrolling_inertia: std::ptr::null_mut(),
            smooth_scrolling_inertia_value: std::ptr::null_mut(),
            mapping_layout_row: 0,
            update_host_row: 0,
            mapping_scroll: std::ptr::null_mut(),
            mapping_doc: std::ptr::null_mut(),
            mapping_card: std::ptr::null_mut(),
            mapping_panel: std::ptr::null_mut(),
            mapping_rows: Vec::new(),
            clipboard_enabled: std::ptr::null_mut(),
            clipboard_shortcut: std::ptr::null_mut(),
            clipboard_shortcut_error: std::ptr::null_mut(),
            clipboard_shortcut_row_height: 0.0,
            clipboard_pin_follow_row_height: 0.0,
            clipboard_row_gap: 0.0,
            key_bindings_enabled: std::ptr::null_mut(),
            key_bindings_doc: std::ptr::null_mut(),
            key_bindings_empty: std::ptr::null_mut(),
            key_bindings_footer: std::ptr::null_mut(),
            key_bindings_footer_button: std::ptr::null_mut(),
            key_bindings_row: 0,
            key_binding_rows: Vec::new(),
            key_binding_items_rendered: Vec::new(),
            clipboard_clear_on_quit: std::ptr::null_mut(),
            clipboard_separate_clear_all: std::ptr::null_mut(),
            clipboard_move_used_to_top: std::ptr::null_mut(),
            clipboard_delete_after_paste: std::ptr::null_mut(),
            clipboard_clear_system_pasteboard_after_paste: std::ptr::null_mut(),
            clipboard_max_entries: std::ptr::null_mut(),
            clipboard_auto_expire_days: std::ptr::null_mut(),
            clipboard_auto_expire_days_value_label: std::ptr::null_mut(),
            clipboard_show_source_app: std::ptr::null_mut(),
            clipboard_pin_follow: std::ptr::null_mut(),
            add_mapping_button: std::ptr::null_mut(),
            mapping_enabled: std::ptr::null_mut(),
            mapping_empty: std::ptr::null_mut(),
            device_indicator: std::ptr::null_mut(),
            device_info_caption: std::ptr::null_mut(),
            restore_defaults: RestoreDefaultsControl::empty(),
            page_restores: std::array::from_fn(|_| RestoreDefaultsControl::empty()),
            page_canvases: std::array::from_fn(|_| PageCanvas::empty()),
            permission_warning_view: std::ptr::null_mut(),
            update_auto_check: std::ptr::null_mut(),
            update_auto_download: std::ptr::null_mut(),
            update_check_button: std::ptr::null_mut(),
            update_host: std::ptr::null_mut(),
            update_host_window: std::ptr::null_mut(),
            update_card: std::ptr::null_mut(),
            update_divider: std::ptr::null_mut(),
            update_card_expanded: false,
        };

        // The sidebar and detail pane meet directly at the original sidebar boundary; their
        // backgrounds provide the visual split instead of an inset outer card.
        let content_x = card_w;
        let detail_w = view_w - content_x;
        // HTML `.settings-content { padding: 0 var(--sp-7) var(--sp-6) }`: 28px horizontal.
        let page_inset = super::SETTINGS_DETAIL_INSET;
        let page_x = content_x + page_inset;
        // A: reserve the *measured* scroller footprint (0 for overlay). If the system forces legacy
        // and B's re-assert does not stick, this keeps the content laid out to the visible width:
        // it merely narrows instead of being clipped.
        let content_w = (detail_w - crate::scroller::reserved() - page_inset * 2.0).max(1.0);
        let layout = SettingsLayout::new(content_w);

        sidebar::build_settings_sidebar(
            content,
            sidebar::SettingsSidebarGeometry {
                // The sidebar begins under the band, where the mock's .settings-layout starts.
                content_h: layout_h,
                view_w,
                card_margin,
                card_w,
                card_radius,
            },
            palette,
            target,
            &mut ui,
        );

        // The scroll view spans from the left gutter to the detail pane's right edge, so the
        // overlay scrollbar sits flush with the window edge (matching the OK/Cancel footer) and
        // no longer floats 32pt in from the right. The content keeps its 32pt gutter margins
        // inside the document, so only the scrollbar's position changes.
        // The page viewport starts at the band's bottom edge: the mock's .settings-layout begins
        // under its 36px .settings-titlebar, so no page content may scroll up under the band (it
        // would draw through the title and read as a transparent bar).
        let page_frame = settings_page_frame(page_x, detail_w - page_inset, band_h, content_h);
        let page_context = page_builder::SettingsPageBuildContext {
            content,
            content_w,
            page_x,
            page_frame,
            page_viewport_h,
            palette,
            layout,
            target,
        };
        let content = page_context.content;
        let page_frame = page_context.page_frame;
        let _ = page_context.palette;
        let target = page_context.target;
        // Build from generous provisional heights. They are intentionally not shrunk after child
        // frames are assigned: the pages use manual top-anchored coordinates, so post-hoc
        // shrinking would let AppKit move the children a second time.
        // Built from generous provisional heights. They are intentionally not shrunk after child
        // frames are assigned: the pages use manual top-anchored coordinates, so post-hoc
        // shrinking would let AppKit move the children a second time. Every height includes
        // SettingsPageHeader's 42pt top padding (18 more than the old 24pt inset).
        // Includes the Backup section: two described rows plus its header.
        let general_doc_h = 1320.0;
        let switcher_doc_h = 1432.0;
        let mouse_doc_h = 1620.0;
        let clipboard_doc_h = 978.0;
        // The Key Bindings page: one card holding the master switch and the dynamic binding rows
        // (the bindings module hands the list's height to the page canvas), plus the add row. The
        // provisional height only has to be generous enough for the build pass; the rows reflow
        // themselves afterwards.
        let key_bindings_doc_h = 1500.0;
        let keystroke_display_doc_h = 650.0;
        let about_doc_h = 1300.0;

        // One page per identity, in `SETTINGS_PAGES` order. The provisional height is looked up by
        // identity, so a table reorder moves the page with its height instead of pairing one page
        // with another page's provisional document height.
        let provisional_doc_h = |page: SettingsPageId| match page {
            SettingsPageId::General => general_doc_h,
            SettingsPageId::Switcher => switcher_doc_h,
            SettingsPageId::Mouse => mouse_doc_h,
            SettingsPageId::Clipboard => clipboard_doc_h,
            SettingsPageId::KeyBindings => key_bindings_doc_h,
            SettingsPageId::KeystrokeDisplay => keystroke_display_doc_h,
            SettingsPageId::About => about_doc_h,
        };
        let mut page_roots = [std::ptr::null_mut::<AnyObject>(); SETTINGS_PAGE_COUNT];
        let mut page_documents = [std::ptr::null_mut::<AnyObject>(); SETTINGS_PAGE_COUNT];
        for (index, page) in settings_page_ids().into_iter().enumerate() {
            // Only General is built visible; every other page is hidden until selected.
            let built = SettingsPage::new(
                content,
                page_frame,
                provisional_doc_h(page),
                page != SettingsPageId::General,
            );
            page_roots[index] = built.scroll;
            page_documents[index] = built.document;
            ui.page_views[settings_page_index(page)] = built.scroll;
        }
        // Record this page's measured footprint for the next build (0 = overlay).
        crate::scroller::note_reserved(crate::scroller::reserved_width(
            page_roots[settings_page_index(SettingsPageId::About)],
        ));

        let page_view = |page: SettingsPageId| page_documents[settings_page_index(page)];
        let general_view = page_view(SettingsPageId::General);
        let switcher_view = page_view(SettingsPageId::Switcher);
        let mouse_view = page_view(SettingsPageId::Mouse);
        let clipboard_view = page_view(SettingsPageId::Clipboard);
        let key_bindings_view = page_view(SettingsPageId::KeyBindings);
        let keystroke_display_view = page_view(SettingsPageId::KeystrokeDisplay);
        let about_view = page_view(SettingsPageId::About);
        // Every row the builders draw is recorded for the sidebar search, in one index per build:
        // a locale or theme rebuild re-records the whole index from the strings it draws again.
        page_builder::begin_settings_search_recording();
        // Each builder's content bottom lands at its own identity's slot, so the layout owner a
        // page is handed cannot be another page's.
        let mut page_bottoms = [0.0f64; SETTINGS_PAGE_COUNT];
        page_bottoms[settings_page_index(SettingsPageId::General)] =
            page_builder::build_general_page(&page_context, general_view, general_doc_h, &mut ui);

        page_bottoms[settings_page_index(SettingsPageId::Switcher)] =
            page_builder::build_switcher_page(
                &page_context,
                switcher_view,
                switcher_doc_h,
                &mut ui,
            );

        page_bottoms[settings_page_index(SettingsPageId::Mouse)] =
            page_builder::build_mouse_page(&page_context, mouse_view, mouse_doc_h, &mut ui);

        // The storage state can be decided before OR after this build (the load runs off the main
        // thread), so the banner is settled from the state after the views exist: reading it inside
        // the builder would miss a state that arrives while the page is being laid out.
        super::dispatch::refresh_clipboard_unavailable_notice();
        page_bottoms[settings_page_index(SettingsPageId::Clipboard)] =
            page_builder::build_clipboard_page(
                &page_context,
                clipboard_view,
                clipboard_doc_h,
                &mut ui,
            );

        page_bottoms[settings_page_index(SettingsPageId::KeyBindings)] =
            page_builder::build_key_bindings_page(
                &page_context,
                key_bindings_view,
                key_bindings_doc_h,
                &mut ui,
            );

        page_bottoms[settings_page_index(SettingsPageId::KeystrokeDisplay)] =
            page_builder::build_keystroke_display_page(
                &page_context,
                keystroke_display_view,
                keystroke_display_doc_h,
                &mut ui,
            );

        page_bottoms[settings_page_index(SettingsPageId::About)] =
            page_builder::build_about_page(&page_context, about_view, about_doc_h, window, &mut ui);

        page_builder::finalize_settings_pages(
            content,
            window,
            page_frame,
            content_w,
            target,
            page_builder::SettingsPageFinalization {
                roots: page_roots,
                documents: page_documents,
                bottoms: page_bottoms,
            },
            &mut ui,
        );

        // The search results stand in for the pages while a query is active; they cover exactly
        // the detail pane's page area and are added after the pages so a click lands on a result.
        // The search results stand in for the pages while a query is active: they cover exactly
        // the detail pane's page area (the same frame the pages were built into) and are added
        // after the pages so a click lands on a result.
        sidebar::build_settings_search_results(&mut ui.settings_search, content, page_frame);

        // --- Numeric text-field notifications: debounced apply while typing, immediate commit
        // on blur / Enter ---
        {
            let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
            for (field_ptr, change_sel, end_sel) in [
                (
                    ui.corner_radius,
                    sel!(handleControlTextDidChange:),
                    sel!(handleControlTextDidEndEditing:),
                ),
                (
                    ui.clipboard_max_entries,
                    sel!(handleControlTextDidChange:),
                    sel!(handleControlTextDidEndEditing:),
                ),
            ] {
                let change_name = make_nsstring("NSControlTextDidChangeNotification");
                let _: () = msg_send![
                    center,
                    addObserver: target,
                    selector: change_sel,
                    name: change_name,
                    object: field_ptr
                ];
                CFRelease(change_name as *const c_void);
                let end_name = make_nsstring("NSControlTextDidEndEditingNotification");
                let _: () = msg_send![
                    center,
                    addObserver: target,
                    selector: end_sel,
                    name: end_name,
                    object: field_ptr
                ];
                CFRelease(end_name as *const c_void);
            }
        }

        // --- Sidebar search field: live filtering, the focused shell ring, and the clear chip ---
        if !ui.settings_search.field.is_null() {
            let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
            for (name, selector) in [
                (
                    "NSControlTextDidChangeNotification",
                    sel!(handleSettingsSearchChanged:),
                ),
                (
                    "NSControlTextDidBeginEditingNotification",
                    sel!(handleSettingsSearchBeganEditing:),
                ),
                (
                    "NSControlTextDidEndEditingNotification",
                    sel!(handleSettingsSearchEndedEditing:),
                ),
            ] {
                let notification_name = make_nsstring(name);
                let _: () = msg_send![
                    center,
                    addObserver: target,
                    selector: selector,
                    name: notification_name,
                    object: ui.settings_search.field
                ];
                CFRelease(notification_name as *const c_void);
            }
        }

        with_settings_ui(|slot| *slot = Some(ui));

        // The window may be built while a page is already selected (SIDEBAR_SELECTED != 0, e.g.
        // launched straight onto About, or opened from the update notification): the page is built
        // accordingly, but the sidebar highlight defaults to item 0. Re-apply the selection once the
        // build has finished. It must run OUTSIDE the with_settings_ui closure: re-borrowing from
        // inside hits MainThreadSlot's reentrancy guard and silently does nothing (measured: that is
        // why the highlight did not move).
        select_sidebar(SIDEBAR_SELECTED.load(Ordering::SeqCst));
    }
}

/// Drop every settings registry that is keyed by raw view address.
///
/// The settings content is built from absolute-positioned views that the code addresses directly
/// (row label, sidebar label/icon/dot, select parts) instead of routing lookups through a model, so
/// an entry is only meaningful while the view it names is alive. Whenever the content hierarchy is
/// replaced -- a full window build, an in-place content rebuild, or teardown -- all of them must be
/// emptied, or a later hit-test messages a freed object and traps in objc's receiver check.
///
/// One function so a newly added address-keyed registry cannot be wired into only some of the
/// paths.
fn clear_settings_content_registries() {
    SettingsRow::clear_runtime_registry();
    sidebar::reset_settings_search_rows();
    widgets::clear_settings_select_registry();
    tooltip::SettingsTooltip::clear_runtime_registries();
    widgets::clear_sidebar_view_registries();
}

/// Detach global references owned by a settings window before it is released or replaced.
unsafe fn detach_settings_window_runtime() {
    // Detach the updater's host references before releasing the window so it never touches
    // a deallocated view.
    crate::updater::clear_update_host();
    clear_settings_content_registries();
}

/// Invalidate the cached settings window (release + set None) so it is rebuilt with the
pub(crate) fn invalidate_settings_window() {
    SYSTEM_APPEARANCE_REBUILD_PENDING.store(false, Ordering::SeqCst);
    let ui = with_settings_ui(|slot| slot.take());
    if let Some(u) = ui {
        unsafe {
            // Before the runtime is detached: the panels' dim layers belong to this window's content
            // view, which is about to be released.
            close_floating_editors();
            detach_settings_window_runtime();
            // The window is alloc +1 with setReleasedWhenClosed:false, so release once manually;
            // its subviews are retained by the parent view and dealloc with the window.
            let _: () = msg_send![u.window, orderOut: std::ptr::null::<AnyObject>()];
            release_obj(u.window);
        }
        // The window is invalidated/destroyed; flip back to .accessory (it may have been open
        // during a locale change).
        crate::set_settings_activation_policy(false);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        abandoned_page_editors, settings_page_frame, warning_for_page, PageEditor, PageWarning,
        SETTINGS_CLIPBOARD_PAGE_INDEX, SETTINGS_KEY_BINDINGS_PAGE_INDEX, SETTINGS_MOUSE_PAGE_INDEX,
    };

    /// The detail pane's page area hangs from the band's bottom edge and fills down to the
    /// window's own bottom edge: it is the frame the pages are built into AND the one the search
    /// results stand in with. Insetting it by the band a second time left a strip of unused pane
    /// below (and put the results' empty state off the page area).
    #[test]
    fn the_page_area_fills_the_pane_below_the_title_bar_band() {
        let frame = settings_page_frame(240.0, 580.0, 36.0, 720.0);
        assert_eq!(frame.origin.x, 240.0);
        assert_eq!(frame.size.width, 580.0);
        // Origin 0 is the window's bottom edge: the pane starts there...
        assert_eq!(frame.origin.y, 0.0);
        // ... and stops at the band's bottom edge.
        assert_eq!(frame.size.height, 720.0 - 36.0);
        assert_eq!(frame.origin.y + frame.size.height, 720.0 - 36.0);
        // A window shorter than its band still yields a usable page.
        assert_eq!(settings_page_frame(0.0, 100.0, 40.0, 30.0).size.height, 1.0);
    }

    /// Switching away from a page has to wrap up that page's own editors, and only those: leaving
    /// the Key Bindings page closes its recording and its add panel but leaves the Mouse page's
    /// mapping panel alone, and vice versa. Getting this wrong is how a floating panel outlives the
    /// page it belongs to.
    #[test]
    fn leaving_a_page_abandons_exactly_that_pages_editors() {
        let bindings = abandoned_page_editors(SETTINGS_KEY_BINDINGS_PAGE_INDEX);
        assert!(
            !bindings.contains(&PageEditor::BindingRecording)
                && !bindings.contains(&PageEditor::BindingPanel),
            "staying on the Key Bindings page must not close its own editors: {bindings:?}"
        );
        assert!(bindings.contains(&PageEditor::MappingPanel));

        let mouse = abandoned_page_editors(SETTINGS_MOUSE_PAGE_INDEX);
        assert!(mouse.contains(&PageEditor::BindingRecording));
        assert!(mouse.contains(&PageEditor::BindingPanel));
        assert!(
            !mouse.contains(&PageEditor::MappingPanel),
            "staying on the Mouse page must not close its own panel: {mouse:?}"
        );

        // Any other page abandons every editor.
        let other = abandoned_page_editors(0);
        assert_eq!(
            other,
            vec![
                PageEditor::BindingRecording,
                PageEditor::BindingPanel,
                PageEditor::MappingPanel
            ]
        );
    }

    /// The permission warning outranks the storage one on General, and a page that has nothing to
    /// report keeps the strip hidden: that is what decides whether the banner covers a page title.
    #[test]
    fn the_banner_copy_follows_the_page_and_the_two_failure_facts() {
        let clipboard = SETTINGS_CLIPBOARD_PAGE_INDEX;
        let granted = true;
        let missing = false;
        let storage_ok = false;
        let storage_down = true;

        assert_eq!(
            warning_for_page(0, granted, storage_down),
            PageWarning::Storage
        );
        assert_eq!(
            warning_for_page(0, missing, storage_down),
            PageWarning::Permission
        );
        assert_eq!(
            warning_for_page(0, missing, storage_ok),
            PageWarning::Permission
        );
        assert_eq!(
            warning_for_page(clipboard, granted, storage_down),
            PageWarning::Storage
        );
        assert_eq!(
            warning_for_page(clipboard, missing, storage_down),
            PageWarning::Storage
        );
        assert_eq!(warning_for_page(0, granted, storage_ok), PageWarning::None);
        assert_eq!(
            warning_for_page(clipboard, granted, storage_ok),
            PageWarning::None
        );
        // Any other page stays clear, whatever the two facts are.
        for page in 1..8 {
            if page == clipboard {
                continue;
            }
            assert_eq!(
                warning_for_page(page, missing, storage_down),
                PageWarning::None
            );
            assert_eq!(
                warning_for_page(page, granted, storage_down),
                PageWarning::None
            );
        }
    }
}
