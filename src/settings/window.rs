//! Settings window classes and lifecycle (show/hide/sidebar switching) plus content construction.

use super::*;

/// Show the permission banner in its own top strip and reserve that strip above General.
unsafe fn set_permission_banner_visible(ui: &SettingsUi, visible: bool) {
    if ui.permission_warning_view.is_null() || ui.general_view.is_null() {
        return;
    }

    let hidden: bool = msg_send![ui.permission_warning_view, isHidden];
    if hidden == !visible {
        return;
    }

    let banner_frame: NSRect = msg_send![ui.permission_warning_view, frame];
    let mut general_frame: NSRect = msg_send![ui.general_view, frame];
    let height_delta = if visible {
        -banner_frame.size.height
    } else {
        banner_frame.size.height
    };
    general_frame.size.height = (general_frame.size.height + height_delta).max(1.0);
    let _: () = msg_send![ui.general_view, setFrame: general_frame];
    let _: () = msg_send![ui.permission_warning_view, setHidden: !visible];
}

/// Switch the active settings page: align the highlight to the selected button, toggle the
/// eight content views' visibility, and bold the selected item's label.
pub(super) fn select_sidebar(idx: usize) {
    // fall back to the General page if the tag is out of range
    let idx = if idx >= SETTINGS_PAGE_COUNT { 0 } else { idx };
    // Dismiss the previous page's disabled hint so the bubble cannot remain across tabs.
    unsafe { tooltip::SettingsTooltip::dismiss() };
    let previous_idx = SIDEBAR_SELECTED.swap(idx, Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui| {
            let ui = match ui.as_ref() {
                Some(u) => u,
                None => return,
            };
            let buttons = [
                ui.sidebar_general,
                ui.sidebar_switcher,
                ui.sidebar_mouse,
                ui.sidebar_clipboard,
                ui.sidebar_window_control,
                ui.sidebar_quick_actions,
                ui.sidebar_keystroke_display,
                ui.sidebar_about,
            ];
            let views = [
                ui.general_view,
                ui.switcher_view,
                ui.mouse_view,
                ui.clipboard_view,
                ui.window_control_view,
                ui.quick_actions_view,
                ui.keystroke_display_view,
                ui.about_view,
            ];
            // align the highlight to the selected button's frame
            let frame: NSRect = msg_send![buttons[idx], frame];
            // When the pointer is already over the target tab, the hover background is in place;
            // clicking only synchronizes selection instead of replaying a conspicuous glide.
            // Keyboard and non-hovered selection changes keep the full spring.
            let target_is_hovered = widgets::sidebar_button_is_hovered(buttons[idx]);
            SettingsSidebar::move_highlight(
                ui.sidebar_highlight,
                frame,
                previous_idx != idx && !target_is_hovered,
            );
            // Selected items use an accent-colored bold title; unselected items use the system label color.
            let titles = [
                t("settings.sidebar_general"),
                t("settings.sidebar_switcher"),
                t("settings.sidebar_mouse"),
                t("settings.sidebar_clipboard"),
                t("settings.sidebar_window_control"),
                t("settings.sidebar_quick_actions"),
                t("settings.sidebar_keystroke_display"),
                t("settings.sidebar_about"),
            ];
            for (i, &b) in buttons.iter().enumerate() {
                let layer: *mut AnyObject = msg_send![b, layer];
                if !layer.is_null() {
                    layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
                }
                set_sidebar_title(b, &titles[i], i == idx);
            }
            // Toggle every page's visibility.
            for (i, &v) in views.iter().enumerate() {
                let _: () = msg_send![v, setHidden: i != idx];
            }
            let show_permission_banner = if idx == 0 {
                let is_permission_migration =
                    crate::update_notice::needs_permission_migration_copy();
                let has_required_permissions = has_accessibility_permission()
                    && (!is_permission_migration || crate::thumbnail::capture_allowed());
                !has_required_permissions
            } else {
                false
            };
            set_permission_banner_visible(ui, show_permission_banner);
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
            let page_names: [&str; SETTINGS_PAGE_COUNT] = [
                "general",
                "switcher",
                "mouse",
                "clipboard",
                "window-control",
                "quick-actions",
                "keystroke-display",
                "about",
            ];
            page.validate(page_names[idx]);
        });
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
    if !ui.screen_recording_permission_status.is_null() {
        set_permission_status(
            ui.screen_recording_permission_status,
            crate::thumbnail::capture_allowed(),
        );
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
                        let is_permission_migration =
                            crate::update_notice::needs_permission_migration_copy();
                        let has_required_permissions = has_accessibility_permission()
                            && (!is_permission_migration || crate::thumbnail::capture_allowed());
                        set_permission_banner_visible(ui, !has_required_permissions);
                    } else {
                        refresh_permission_statuses(ui);
                    }
                }
            }
        }
    });
}

/// Traffic-light offset: restore the original down-right position.
/// Window coordinates point up, so down-right = x+ / y-.
const TRAFFIC_LIGHT_DX: f64 = 8.0;
const TRAFFIC_LIGHT_DY: f64 = -6.0;
static TRAFFIC_LIGHT_BASE_ORIGINS: LazyLock<Mutex<HashMap<usize, [Option<NSPoint>; 3]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Nudge the three traffic-light buttons down-right: grab the button views via the public
/// -standardWindowButton: and move their frames (no public API sets the traffic light position;
/// the old private setTrafficLightPosition: etc. are gone on macOS 26, and this is the only
/// reliable way -- verified on this machine). Note: the two-arg +standardWindowButton:forStyleMask:
/// is a CLASS method; sending it to an instance trips objc2's method check and panics (a pitfall
/// we hit) -- the one-arg instance method -standardWindowButton: must be used. Must run after the
/// window's first layout pass: moves before layout are reset by AppKit, and resize also resets
/// them, so the offset is re-applied on every show and resize (see show_settings and
/// resizeSubviewsWithOldSize:). Original positions are captured once per window so repeated
/// calls remain idempotent instead of accumulating the offset. Skips silently when a button is
/// nil; works on older macOS too.
pub(super) unsafe fn reposition_traffic_lights(window: *mut AnyObject) {
    // NSWindowButton: Close=0, Miniaturize=1, Zoom=2
    let base_origins = {
        let mut all_origins = TRAFFIC_LIGHT_BASE_ORIGINS.lock().unwrap();
        let origins = all_origins
            .entry(window as usize)
            .or_insert([None, None, None]);
        for tag in 0..3isize {
            let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
            if !btn.is_null() && origins[tag as usize].is_none() {
                let f: NSRect = msg_send![btn, frame];
                origins[tag as usize] = Some(f.origin);
            }
        }
        *origins
    };
    for tag in 0..3isize {
        let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
        if let (false, Some(base_origin)) = (btn.is_null(), base_origins[tag as usize]) {
            let _: () = msg_send![
                btn,
                setFrameOrigin: NSPoint::new(
                    base_origin.x + TRAFFIC_LIGHT_DX,
                    base_origin.y + TRAFFIC_LIGHT_DY
                )
            ];
        }
    }
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
        for (index, scroll) in [
            ui.general_view,
            ui.switcher_view,
            ui.mouse_view,
            ui.clipboard_view,
            ui.window_control_view,
            ui.quick_actions_view,
            ui.keystroke_display_view,
            ui.about_view,
        ]
        .into_iter()
        .enumerate()
        {
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

unsafe fn capture_settings_scroll_offsets(ui: &SettingsUi) -> [NSPoint; SETTINGS_PAGE_COUNT] {
    let scrolls = [
        ui.general_view,
        ui.switcher_view,
        ui.mouse_view,
        ui.clipboard_view,
        ui.window_control_view,
        ui.quick_actions_view,
        ui.keystroke_display_view,
        ui.about_view,
    ];
    scrolls.map(|scroll| {
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let bounds: NSRect = msg_send![clip, bounds];
        bounds.origin
    })
}

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

fn show_settings_inner(
    preserved_frame: Option<NSRect>,
    page: usize,
    preserved_scroll_offsets: Option<[NSPoint; SETTINGS_PAGE_COUNT]>,
    present_window: bool,
) {
    unsafe {
        {
            let missing = with_settings_ui(|ui| ui.is_none());
            if missing {
                create_settings_window();
            }
        }
        // Always reopen in the collapsed state so no unfinished confirmation lingers (both cards).
        collapse_restore_confirmations(false);
        // A window order-out is not guaranteed to deliver mouseExited for its tracking areas;
        // clear the shared hover state before reusing the settings window.
        widgets::clear_sidebar_hover();
        load_settings_values();
        // Normal opens return to General; seamless refreshes preserve the current page.
        select_sidebar(page);
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
                // Offset the traffic lights only after the window's first layout pass, or AppKit
                // resets them.
                let _: () = msg_send![u.window, layoutIfNeeded];
                reposition_traffic_lights(u.window);
                if let Some(offsets) = preserved_scroll_offsets {
                    let scrolls = [
                        u.general_view,
                        u.switcher_view,
                        u.mouse_view,
                        u.clipboard_view,
                        u.window_control_view,
                        u.quick_actions_view,
                        u.keystroke_display_view,
                        u.about_view,
                    ];
                    for (scroll, origin) in scrolls.into_iter().zip(offsets) {
                        restore_settings_scroll_offset(scroll, origin);
                    }
                } else {
                    // Scroll the visible page (General on open) to the top after layout, so the
                    // scrollbar starts at the top of the track instead of the middle.
                    scroll_page_to_top(u.general_view);
                }
                if present_window {
                    // Clear the default first responder so focus does not land on the Glass color control on open.
                    let _: bool =
                        msg_send![u.window, makeFirstResponder: std::ptr::null::<AnyObject>()];
                    set_text_input_active(false);
                }
                // The migration copy requires both permissions; the regular copy follows Accessibility only.
                let is_permission_migration =
                    crate::update_notice::needs_permission_migration_copy();
                let has_required_permissions = has_accessibility_permission()
                    && (!is_permission_migration || crate::thumbnail::capture_allowed());
                let show_permission_banner =
                    SIDEBAR_SELECTED.load(Ordering::SeqCst) == 0 && !has_required_permissions;
                set_permission_banner_visible(u, show_permission_banner);
            }
        });
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
                    let scrolls = [
                        u.general_view,
                        u.switcher_view,
                        u.mouse_view,
                        u.clipboard_view,
                        u.window_control_view,
                        u.quick_actions_view,
                        u.keystroke_display_view,
                        u.about_view,
                    ];
                    widgets::scroll_page_to_top(scrolls[selected]);
                }
            });
        }
    }
}

pub(super) fn hide_settings() {
    // Defensive: wrap up any in-progress recording / edit panel when the window closes.
    cancel_recording_from_main();
    close_mapping_panel();
    set_text_input_active(false);
    // Closing settings discards any unconfirmed restore action and resets to one button (both
    // cards).
    collapse_restore_confirmations(false);
    let window_and_well = with_settings_ui(|ui| ui.as_ref().map(|u| (u.window, u.glass_tint)));
    unsafe {
        if let Some((window, well)) = window_and_well {
            // orderOut can bypass the sidebar tracking-area exit callback, so do not leave the
            // shared hover pill pointing at a row while the window is hidden.
            widgets::clear_sidebar_hover();
            // Release the settings lock before closing the color panel; its notification callback
            // re-enters SETTINGS_UI.
            close_glass_tint_panel(well);
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
        widgets::clear_sidebar_hover();
        detach_settings_window_runtime(&old_ui, false);
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
        let document: *mut AnyObject = msg_send![ui.about_view, documentView];
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

/// Whether the expanded update card and its host sit inside the page document with the page's bottom
/// padding, and the document covers the card's top edge (i.e. the whole card can be scrolled to).
unsafe fn update_card_inside_document() -> bool {
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return false;
        };
        let document: *mut AnyObject = msg_send![ui.about_view, documentView];
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
                doc_frame.size.height, card, host, super::UPDATE_CARD_BOTTOM_PADDING
            );
        }
        ok
    })
}

/// The inline update flow's ownership state as the layout leaves it: what the flow itself hid,
/// where its divider sits relative to the row above it, and how the card hugs the host.
#[derive(Debug)]
struct UpdateFlowState {
    document_height: f64,
    button_hidden: bool,
    host_hidden: bool,
    divider_from_row_above: f64,
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
        let document: *mut AnyObject = msg_send![ui.about_view, documentView];
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
    unsafe {
        show_settings();
        // Regression guard for the guide's "Open App Settings" crash (2026-09-28): the settings
        // window becoming key delivers the scroller notification synchronously from inside
        // `with_settings_ui`, and the resync re-entered the borrow ("RefCell already borrowed" ->
        // abort). Driving the real handler with an active borrow must defer, not panic; without the
        // guard this kills the smoke process and the test fails.
        {
            let controller = crate::CONTROLLER.lock().unwrap().map(|target| target.0);
            if let Some(controller) = controller {
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
                // callback and abort. Both paths must survive.
                crate::on_scroller_style_changed(
                    controller,
                    sel!(handleScrollerStyleChanged:),
                    std::ptr::null_mut(),
                );
            }
        }
        let Some((window, pages)) = with_settings_ui(|ui| {
            ui.as_ref().map(|ui| {
                (
                    ui.window,
                    [
                        ui.general_view,
                        ui.switcher_view,
                        ui.mouse_view,
                        ui.clipboard_view,
                        ui.window_control_view,
                        ui.quick_actions_view,
                        ui.keystroke_display_view,
                        ui.about_view,
                    ],
                )
            })
        }) else {
            return false;
        };
        let sidebar_layout_ok = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let buttons = [
                ui.sidebar_general,
                ui.sidebar_switcher,
                ui.sidebar_mouse,
                ui.sidebar_clipboard,
                ui.sidebar_window_control,
                ui.sidebar_quick_actions,
                ui.sidebar_keystroke_display,
                ui.sidebar_about,
            ];
            if buttons.iter().any(|button| button.is_null()) {
                return Some(false);
            }
            let parent: *mut AnyObject = msg_send![buttons[0], superview];
            if parent.is_null() {
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
                    if (step - (previous_frame.size.height + 4.0)).abs() > 0.5 {
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
        if !root.is_null() && AnyClass::get(c"NSGlassEffectView").is_some() {
            let children: *mut AnyObject = msg_send![root, subviews];
            let child_count: usize = msg_send![children, count];
            let mut glass_content_ok = false;
            for index in 0..child_count {
                let child: *mut AnyObject = msg_send![children, objectAtIndex: index as isize];
                if msg_send![child, isKindOfClass: class!(NSGlassEffectView)] {
                    let glass_content: *mut AnyObject = msg_send![child, contentView];
                    glass_content_ok = !glass_content.is_null();
                    break;
                }
            }
            if !glass_content_ok {
                log_info!("[smoke-settings-layout] Liquid Glass sidebar has no contentView");
                hide_settings();
                return false;
            }
        }
        let names = [
            "general",
            "switcher",
            "mouse",
            "clipboard",
            "window-control",
            "quick-actions",
            "keystroke-display",
            "about",
        ];
        for (index, page) in pages.iter().enumerate() {
            select_sidebar(index);
            let _: () = msg_send![window, layoutIfNeeded];
            scroll_page_to_top(*page);
            debug_validate_settings_page(*page, names[index]);
        }
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
        crate::settings::expand_update_section(600.0);
        let _: () = msg_send![window, layoutIfNeeded];
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
        crate::settings::expand_update_section(600.0);
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
        crate::settings::expand_update_section(600.0);
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
        let inline_checks: [(&str, bool); 14] = [
            ("inside", first_inside && second_inside && third_inside),
            (
                "expand_grows",
                expanded.document_height > compact.document_height + 500.0,
            ),
            ("collapse_restored", page_restored(&compact, &collapsed)),
            (
                "tall_expand_grows",
                tall_expanded.document_height > tall_compact.document_height + 500.0,
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
            // The card follows the host's bottom edge with the shared card inset (10pt), exactly
            // like every other card follows its last row.
            (
                "card_hugs_host",
                (expanded_state.card_rect_bottom_from_host_bottom + 4.0).abs() <= 0.5
                    && (collapsed_state.card_rect_bottom_from_host_bottom + 4.0).abs() <= 0.5,
            ),
            // The Update divider belongs to the check-button row (the boundary above the button), so
            // it keeps its distance to the row above it in every state: the switch sits 16pt above it.
            (
                "divider_glued_to_row_above",
                [&compact_state, &expanded_state, &collapsed_state]
                    .iter()
                    .all(|state| (state.divider_from_row_above + 16.0).abs() <= 0.5),
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
        cfg.layout.thumbnails_enabled = true;
        cfg.layout.focused_thumbnail_prewarm = true;
        cfg.layout.show_app_name_in_cards = true;
        cfg.keystroke_display.enabled = true;
        cfg.keystroke_display.mode = "commands".into();
        cfg.keystroke_display.tap_level = "hid".into();
        cfg.keystroke_display.follow_frontmost_screen = true;
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg.clone();
        }

        show_settings();
        rebuild_settings_content_now();
        select_sidebar(SETTINGS_KEYSTROKE_DISPLAY_PAGE_INDEX);

        let states = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let keystroke_page_document: *mut AnyObject =
                msg_send![ui.keystroke_display_view, documentView];
            let controls_on_keystroke_page = [
                ui.keystroke_display_enabled,
                ui.keystroke_display_mode,
                ui.keystroke_display_tap_level,
                ui.keystroke_display_follow_screen,
            ]
            .into_iter()
            .all(|control| {
                if control.is_null() {
                    return false;
                }
                let parent: *mut AnyObject = msg_send![control, superview];
                parent == keystroke_page_document
            });
            Some((
                msg_send![ui.windows_enabled, state],
                msg_send![ui.show_minimized, state],
                msg_send![ui.show_hidden_app_windows, state],
                msg_send![ui.thumbnails_enabled, indexOfSelectedItem],
                msg_send![ui.focused_thumbnail_prewarm, state],
                msg_send![ui.show_app_name_in_cards, state],
                msg_send![ui.keystroke_display_enabled, state],
                msg_send![ui.keystroke_display_mode, indexOfSelectedItem],
                msg_send![ui.keystroke_display_tap_level, indexOfSelectedItem],
                msg_send![ui.keystroke_display_follow_screen, state],
                controls_on_keystroke_page,
            ))
        });

        let mut refreshed_cfg = cfg;
        refreshed_cfg.keystroke_display.enabled = false;
        refreshed_cfg.keystroke_display.mode = "shortcuts".into();
        refreshed_cfg.keystroke_display.tap_level = "session".into();
        refreshed_cfg.keystroke_display.follow_frontmost_screen = false;
        if let Ok(mut current) = CONFIG.write() {
            *current = refreshed_cfg;
        }
        refresh_switcher_and_keystroke_display_controls_from_config();
        let refreshed = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            Some((
                msg_send![ui.keystroke_display_enabled, state],
                msg_send![ui.keystroke_display_mode, indexOfSelectedItem],
                msg_send![ui.keystroke_display_tap_level, indexOfSelectedItem],
                msg_send![ui.keystroke_display_follow_screen, state],
            ))
        });
        hide_settings();

        states
            == Some((
                1isize, 1isize, 1isize, 1isize, 1isize, 1isize, 1isize, 2isize, 1isize, 1isize,
                true,
            ))
            && refreshed == Some((0isize, 1isize, 0isize, 0isize))
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
            let document: *mut AnyObject = msg_send![ui.clipboard_view, documentView];
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
            let document: *mut AnyObject = msg_send![ui.clipboard_view, documentView];
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
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
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
                ui.page_canvases[2].set_group_visible(RowGroup::LineCount, false);
                ui.page_canvases[2].set_group_visible(RowGroup::PointerAccel, false);
            }
        });
        let mouse_collapsed = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
            let document_frame: NSRect = msg_send![document, frame];
            let line_hidden: bool = msg_send![ui.line_count, isHidden];
            let pointer_hidden: bool = msg_send![ui.pointer_accel_slider, isHidden];
            Some((document_frame.size.height, line_hidden, pointer_hidden))
        });
        // Reopen only the upper group.
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.page_canvases[2].set_group_visible(RowGroup::LineCount, true);
            }
        });
        let mouse_after_upper = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
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
                ui.page_canvases[2].set_group_visible(RowGroup::PointerAccel, true);
            }
        });
        let mouse_document_restored = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
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
#[cfg(test)]
pub(super) use chrome::settings_effective_corner_radius;
pub(super) use chrome::{
    apply_settings_root_surface, apply_settings_window_appearance, refresh_settings_root_corner,
    settings_root_view_class, settings_root_view_for_host, settings_window_class,
};

fn create_settings_window() {
    create_settings_window_for(None);
}

/// Rebuilds the settings content (after a scroller-style change, to fit the new visible width).
pub(crate) unsafe fn rebuild_settings_content_now() {
    if let Some(window) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)) {
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
    unsafe {
        let palette = settings_palette();
        // Keep the original compact settings window dimensions while applying the redesign's
        // typography, spacing, controls, and grouped-card treatment.
        // Give the redesigned detail pane enough room for full labels, links, and wide fields
        // while keeping the sidebar and the existing window height unchanged.
        let view_w = 820.0;
        let card_margin = 0.0;
        let card_w = SETTINGS_SIDEBAR_WIDTH;
        let window_clip_radius = 26.0;
        let card_radius = 0.0;
        let style: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
        // titled + closable + miniaturizable + resizable (all three traffic lights). resizable is
        // required for the green zoom button to appear; the layout is absolute-positioned and
        // doesn't adapt, so the window size is fixed below via min=max.
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
        let ns_title = make_nsstring(&t("settings.window_title"));
        let _: () = msg_send![window, setTitle: ns_title];
        CFRelease(ns_title as *const c_void);
        if existing_window.is_none() {
            let _: () = msg_send![window, setReleasedWhenClosed: false];
            // Fixed width: min and max width both equal the designed width, height stays adjustable --
            // same as System Settings (the width cannot be dragged).
            let _: () = msg_send![window, setMinSize: NSSize::new(view_w, 400.0)];
            let _: () = msg_send![window, setMaxSize: NSSize::new(view_w, 10000.0)];

            // Empty unified toolbar: NSWindowToolbarStyleUnified (3) raises the theme frame's corner
            // radius from 16 to 26 (measured; that's how LinearMouse's settings window does it), and
            // the top strip becomes a glass material strip with the traffic lights centered in it.
            // An empty toolbar adds no responders, so performKeyEquivalent: (Cmd+Q) and page switching
            // are unaffected. Must be set before measuring contentLayoutRect, which then automatically
            // accounts for the toolbar strip (658 -> 624).
            let tb: *mut AnyObject = msg_send![class!(NSToolbar), alloc];
            let tb_id = make_nsstring("OhMyTabSettingsToolbar");
            let tb: *mut AnyObject = msg_send![tb, initWithIdentifier: tb_id];
            CFRelease(tb_id as *const c_void);
            let _: () = msg_send![window, setToolbar: tb];
            let _: () = msg_send![window, setToolbarStyle: 3isize]; // NSWindowToolbarStyleUnified
            release_obj(tb);
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
            tooltip::SettingsTooltip::clear_runtime_registries();
            let subviews: *mut AnyObject = msg_send![content, subviews];
            let count: usize = msg_send![subviews, count];
            for index in (0..count).rev() {
                let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
                let _: () = msg_send![subview, removeFromSuperview];
            }
        }
        // content_h is only used for full-height containers (they cover the whole window after
        // the mask flip); top-anchored rows use layout_h measured after the flip (see below).
        let content_frame: NSRect = msg_send![content, frame];
        let content_h = content_frame.size.height;

        // Remove the hairline under the traffic lights: with fullSizeContentView + a transparent
        // title bar the content extends into the title bar and AppKit stops drawing the separator;
        // the title text is hidden (System Settings look). The contentView must NOT be measured
        // before the flip -- an unlaid-out window reports the full height (690 on macOS 26 in
        // practice). After the flip, contentLayoutRect (macOS 11+; min target is 11.0) gives the
        // real layout area below the traffic-light strip; .min() guards a degenerate result.
        // All top-anchored rows are laid out against layout_h so nothing collides with the lights.
        let _: () = msg_send![window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![window, setStyleMask: style | (1 << 15)]; // NSWindowStyleMaskFullSizeContentView
        let _: () = msg_send![window, setTitleVisibility: 1isize]; // NSWindowTitleHidden
                                                                   // The scroll region spans the full window height (fullSizeContentView), so the content
                                                                   // scrolls all the way up behind the traffic-light strip. There is no bottom footer bar
                                                                   // anymore: each page embeds its own restore control at the end of its content.
        let page_viewport_h = content_h;

        // The root surface owns the window background and clipping. On macOS 27 it follows the
        // system's toolbar-window radius through container concentricity; older systems use the
        // measured fallback radius.
        apply_settings_root_surface(window, content, palette, window_clip_radius);

        let mut ui = SettingsUi {
            window,
            sidebar_general: std::ptr::null_mut(),
            sidebar_switcher: std::ptr::null_mut(),
            sidebar_mouse: std::ptr::null_mut(),
            sidebar_clipboard: std::ptr::null_mut(),
            sidebar_window_control: std::ptr::null_mut(),
            sidebar_quick_actions: std::ptr::null_mut(),
            sidebar_keystroke_display: std::ptr::null_mut(),
            sidebar_about: std::ptr::null_mut(),
            sidebar_highlight: std::ptr::null_mut(),
            general_view: std::ptr::null_mut(),
            switcher_view: std::ptr::null_mut(),
            mouse_view: std::ptr::null_mut(),
            clipboard_view: std::ptr::null_mut(),
            window_control_view: std::ptr::null_mut(),
            quick_actions_view: std::ptr::null_mut(),
            keystroke_display_view: std::ptr::null_mut(),
            about_view: std::ptr::null_mut(),
            about_subtitle: std::ptr::null_mut(),
            accessibility_permission_status: std::ptr::null_mut(),
            accessibility_permission_button: std::ptr::null_mut(),
            screen_recording_permission_status: std::ptr::null_mut(),
            theme: std::ptr::null_mut(),
            glass_style: std::ptr::null_mut(),
            glass_tint: std::ptr::null_mut(),
            glass_preview_switcher: std::ptr::null_mut(),
            glass_preview_clipboard: std::ptr::null_mut(),
            corner_radius: std::ptr::null_mut(),
            thumbnails_enabled: std::ptr::null_mut(),
            focused_thumbnail_prewarm: std::ptr::null_mut(),
            show_app_name_in_cards: std::ptr::null_mut(),
            card_text_size: std::ptr::null_mut(),
            card_text_size_value_label: std::ptr::null_mut(),
            status_bar_text_size: std::ptr::null_mut(),
            status_bar_text_size_value_label: std::ptr::null_mut(),
            modifier: std::ptr::null_mut(),
            locale: std::ptr::null_mut(),
            show_minimized: std::ptr::null_mut(),
            show_hidden_app_windows: std::ptr::null_mut(),
            windows_enabled: std::ptr::null_mut(),
            overlay_position: std::ptr::null_mut(),
            activation_mode: std::ptr::null_mut(),
            keystroke_display_enabled: std::ptr::null_mut(),
            keystroke_display_mode: std::ptr::null_mut(),
            keystroke_display_tap_level: std::ptr::null_mut(),
            keystroke_display_follow_screen: std::ptr::null_mut(),
            log_level: std::ptr::null_mut(),
            launch_at_login: std::ptr::null_mut(),
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
            mapping_layout_row: 0,
            update_host_row: 0,
            mapping_scroll: std::ptr::null_mut(),
            mapping_doc: std::ptr::null_mut(),
            mapping_card: std::ptr::null_mut(),
            mapping_panel: std::ptr::null_mut(),
            mapping_rows: Vec::new(),
            clipboard_enabled: std::ptr::null_mut(),
            window_control_enabled: std::ptr::null_mut(),
            window_control_up: std::ptr::null_mut(),
            window_control_down: std::ptr::null_mut(),
            window_control_left: std::ptr::null_mut(),
            window_control_right: std::ptr::null_mut(),
            window_control_display_up: std::ptr::null_mut(),
            window_control_display_down: std::ptr::null_mut(),
            window_control_display_left: std::ptr::null_mut(),
            window_control_display_right: std::ptr::null_mut(),
            quick_actions_enabled: std::ptr::null_mut(),
            quick_actions_open_settings: std::ptr::null_mut(),
            quick_actions_open_finder: std::ptr::null_mut(),
            quick_actions_show_desktop: std::ptr::null_mut(),
            quick_actions_lock_screen: std::ptr::null_mut(),
            quick_actions_locate_pointer: std::ptr::null_mut(),
            clipboard_persist: std::ptr::null_mut(),
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
            update_card_shadow: std::ptr::null_mut(),
            update_divider: std::ptr::null_mut(),
            update_card_expanded: false,
        };

        // The sidebar and detail pane meet directly at the original sidebar boundary; their
        // backgrounds provide the visual split instead of an inset outer card.
        let content_x = card_w;
        let detail_w = view_w - content_x;
        let page_inset = 32.0;
        let page_x = content_x + page_inset;
        // A: reserve the *measured* scroller footprint (0 for overlay). If the system forces legacy
        // and B's re-assert does not stick, this keeps the content laid out to the visible width:
        // it merely narrows instead of being clipped.
        let content_w = (detail_w - crate::scroller::reserved() - page_inset * 2.0).max(1.0);
        let layout = SettingsLayout::new(content_w);

        let target = match *MENU_TARGET.lock().unwrap() {
            Some(t) => t.0,
            None => return,
        };

        sidebar::build_settings_sidebar(
            content,
            sidebar::SettingsSidebarGeometry {
                content_h,
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
        let page_frame = NSRect::new(
            NSPoint::new(page_x, 0.0),
            NSSize::new(detail_w - page_inset, page_viewport_h),
        );
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
        let general_doc_h = 1138.0;
        let switcher_doc_h = 1432.0;
        let mouse_doc_h = 1620.0;
        let clipboard_doc_h = 978.0;
        // The window-control page contains the master, four direction switches, and four
        // cross-display switches.
        let window_control_doc_h = 1102.0;
        // Quick-actions page: master plus five action switches, mirroring the window-control
        // page.
        let quick_actions_doc_h = 854.0;
        let keystroke_display_doc_h = 650.0;
        let about_doc_h = 1300.0;

        let general_page = SettingsPage::new(content, page_frame, general_doc_h, false);
        let general_root = general_page.scroll;
        let general_view = general_page.document;
        ui.general_view = general_root;
        let switcher_page = SettingsPage::new(content, page_frame, switcher_doc_h, true);
        let switcher_root = switcher_page.scroll;
        let switcher_view = switcher_page.document;
        ui.switcher_view = switcher_root;
        let mouse_page = SettingsPage::new(content, page_frame, mouse_doc_h, true);
        let mouse_root = mouse_page.scroll;
        let mouse_view = mouse_page.document;
        ui.mouse_view = mouse_root;
        let clipboard_page = SettingsPage::new(content, page_frame, clipboard_doc_h, true);
        let clipboard_root = clipboard_page.scroll;
        let clipboard_view = clipboard_page.document;
        ui.clipboard_view = clipboard_root;
        let window_control_page =
            SettingsPage::new(content, page_frame, window_control_doc_h, true);
        let window_control_root = window_control_page.scroll;
        let window_control_view = window_control_page.document;
        ui.window_control_view = window_control_root;
        let quick_actions_page = SettingsPage::new(content, page_frame, quick_actions_doc_h, true);
        let quick_actions_root = quick_actions_page.scroll;
        let quick_actions_view = quick_actions_page.document;
        ui.quick_actions_view = quick_actions_root;
        let keystroke_display_page =
            SettingsPage::new(content, page_frame, keystroke_display_doc_h, true);
        let keystroke_display_root = keystroke_display_page.scroll;
        let keystroke_display_view = keystroke_display_page.document;
        ui.keystroke_display_view = keystroke_display_root;
        let about_page = SettingsPage::new(content, page_frame, about_doc_h, true);
        let about_root = about_page.scroll;
        ui.about_view = about_root;
        // Record this page's measured footprint for the next build (0 = overlay).
        crate::scroller::note_reserved(crate::scroller::reserved_width(about_root));
        let about_view = about_page.document;
        let general_content_bottom =
            page_builder::build_general_page(&page_context, general_view, general_doc_h, &mut ui);

        let switcher_content_bottom = page_builder::build_switcher_page(
            &page_context,
            switcher_view,
            switcher_doc_h,
            &mut ui,
        );

        let mouse_content_bottom =
            page_builder::build_mouse_page(&page_context, mouse_view, mouse_doc_h, &mut ui);

        let clipboard_options_card_bottom = page_builder::build_clipboard_page(
            &page_context,
            clipboard_view,
            clipboard_doc_h,
            &mut ui,
        );

        let window_control_shortcuts_card_bottom = page_builder::build_window_control_page(
            &page_context,
            window_control_view,
            window_control_doc_h,
            &mut ui,
        );

        let quick_actions_card_bottom = page_builder::build_quick_actions_page(
            &page_context,
            quick_actions_view,
            quick_actions_doc_h,
            &mut ui,
        );

        let keystroke_display_card_bottom = page_builder::build_keystroke_display_page(
            &page_context,
            keystroke_display_view,
            keystroke_display_doc_h,
            &mut ui,
        );

        let compact_card_bottom =
            page_builder::build_about_page(&page_context, about_view, about_doc_h, window, &mut ui);

        page_builder::finalize_settings_pages(
            content,
            window,
            page_frame,
            content_w,
            target,
            page_builder::SettingsPageFinalization {
                roots: [
                    general_root,
                    switcher_root,
                    mouse_root,
                    clipboard_root,
                    window_control_root,
                    quick_actions_root,
                    keystroke_display_root,
                    about_root,
                ],
                documents: [
                    general_view,
                    switcher_view,
                    mouse_view,
                    clipboard_view,
                    window_control_view,
                    quick_actions_view,
                    keystroke_display_view,
                    about_view,
                ],
                bottoms: [
                    general_content_bottom,
                    switcher_content_bottom,
                    mouse_content_bottom,
                    clipboard_options_card_bottom,
                    window_control_shortcuts_card_bottom,
                    quick_actions_card_bottom,
                    keystroke_display_card_bottom,
                    compact_card_bottom,
                ],
            },
            &mut ui,
        );

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

/// Detach global references owned by a settings window before it is released or replaced.
/// Keep the traffic-light baseline for in-place redraws; remove it only when the window is
/// actually released.
unsafe fn detach_settings_window_runtime(ui: &SettingsUi, remove_traffic_light_origin: bool) {
    close_glass_tint_panel(ui.glass_tint);
    if remove_traffic_light_origin {
        TRAFFIC_LIGHT_BASE_ORIGINS
            .lock()
            .unwrap()
            .remove(&(ui.window as usize));
    }
    // Detach the updater's host references before releasing the window so it never touches
    // a deallocated view.
    crate::updater::clear_update_host();
    SettingsRow::clear_runtime_registry();
    widgets::clear_settings_select_registry();
    tooltip::SettingsTooltip::clear_runtime_registries();
    SIDEBAR_TITLE_LABELS.lock().unwrap().clear();
    SIDEBAR_ICON_VIEWS.lock().unwrap().clear();
    SIDEBAR_UPDATE_DOTS.lock().unwrap().clear();
}

/// Invalidate the cached settings window (release + set None) so it is rebuilt with the
pub(crate) fn invalidate_settings_window() {
    SYSTEM_APPEARANCE_REBUILD_PENDING.store(false, Ordering::SeqCst);
    set_text_input_active(false);
    let ui = with_settings_ui(|slot| slot.take());
    if let Some(u) = ui {
        unsafe {
            detach_settings_window_runtime(&u, true);
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
