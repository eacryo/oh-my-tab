//! A1 GUI smoke for the Finder Menu settings page.
//!
//! This runner exists because the shared all-pages layout smoke cannot reach its page loop on this
//! tree: an earlier clipboard-page assertion fails first (recorded in docs/review-backlog.md). It
//! opens the real settings window, selects the Finder Menu page and asserts the page's own contract
//! against the live AppKit view tree: every declared control exists and is laid out inside the page
//! document, the master switch gates the controls under it, and a terminal this machine does not
//! have is visibly disabled.
//!
//! Deliberately small: it asserts what this page promises, not the design system (that stays the
//! all-pages layout smoke's job once its earlier failure is fixed).

use objc2::msg_send;
use objc2::runtime::AnyObject;
use objc2_foundation::NSRect;

use super::*;

/// Whether the view is non-null, has a real frame and sits inside its container's bounds.
unsafe fn laid_out(container: *mut AnyObject, view: *mut AnyObject) -> bool {
    if view.is_null() {
        return false;
    }
    let frame: NSRect = msg_send![view, frame];
    let bounds: NSRect = msg_send![container, bounds];
    frame.size.width > 0.0
        && frame.size.height > 0.0
        && frame.origin.x >= -0.5
        && frame.origin.y >= -0.5
        && frame.origin.x + frame.size.width <= bounds.size.width + 0.5
        && frame.origin.y + frame.size.height <= bounds.size.height + 0.5
}

/// The row's label and its control must not intersect: the control is either to the right of the
/// label or on another line.
unsafe fn row_does_not_overlap(control: *mut AnyObject) -> bool {
    let Some(label) = SettingsRow::label_for(control) else {
        return false;
    };
    let label_frame: NSRect = msg_send![label, frame];
    let control_frame: NSRect = msg_send![control, frame];
    let beside = control_frame.origin.x >= label_frame.origin.x + label_frame.size.width - 0.5;
    let above = control_frame.origin.y >= label_frame.origin.y + label_frame.size.height - 0.5;
    let below = label_frame.origin.y >= control_frame.origin.y + control_frame.size.height - 0.5;
    beside || above || below
}

/// Whether a grouped card's internal divider exists between two rows.
///
/// The divider is a 1pt NSView spanning the card width (widgets::add_row_separator), so the check
/// is geometric and orientation-agnostic: the line's y must fall between the two rows' label
/// frames, whichever way the document is flipped. A card whose rows were laid out without a
/// separator fails this even though every row itself is present.
unsafe fn has_separator_between(
    container: *mut AnyObject,
    upper_row: *mut AnyObject,
    lower_row: *mut AnyObject,
    content_w: f64,
) -> bool {
    let (Some(upper), Some(lower)) = (
        SettingsRow::label_for(upper_row),
        SettingsRow::label_for(lower_row),
    ) else {
        return false;
    };
    let upper_frame: NSRect = msg_send![upper, frame];
    let lower_frame: NSRect = msg_send![lower, frame];
    let lo = upper_frame.origin.y.min(lower_frame.origin.y);
    let hi = (upper_frame.origin.y + upper_frame.size.height)
        .max(lower_frame.origin.y + lower_frame.size.height);
    let subviews: *mut AnyObject = msg_send![container, subviews];
    if subviews.is_null() {
        return false;
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if view.is_null() {
            continue;
        }
        let is_plain: bool = msg_send![view, isKindOfClass: objc2::class!(NSView)];
        if !is_plain {
            continue;
        }
        let frame: NSRect = msg_send![view, frame];
        // Cards inset their dividers, so "full width" is a generous fraction of the document.
        if frame.size.height > 1.5 || frame.size.width < content_w * 0.5 {
            continue;
        }
        if frame.origin.y > lo && frame.origin.y < hi {
            return true;
        }
    }
    false
}
/// The value a select renders. The control keeps its text in a dedicated child label (so the arrow
/// keeps its own column), not in the button title.
unsafe fn select_value(select: *mut AnyObject) -> String {
    let subviews: *mut AnyObject = msg_send![select, subviews];
    if subviews.is_null() {
        return String::new();
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if view.is_null() {
            continue;
        }
        let is_text: bool = msg_send![view, isKindOfClass: objc2::class!(NSTextField)];
        if !is_text {
            continue;
        }
        let value: *mut AnyObject = msg_send![view, stringValue];
        let text = crate::ffi::nsstring_to_rust(value);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

pub(crate) fn run() -> bool {
    unsafe {
        log_info!("[smoke-finder-menu] opening settings on the Finder Menu page");
        show_settings_page(settings_page_index(SettingsPageId::FinderMenu));
        let mut ok = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let page = ui.page_view(SettingsPageId::FinderMenu);
            if page.is_null() {
                log_info!("[smoke-finder-menu] the Finder Menu page was not built");
                return Some(false);
            }
            // Rows are subviews of the page's (flipped) document view, so their frames are in the
            // document's coordinate space; the scroll view only holds the viewport.
            let document: *mut AnyObject = msg_send![page, documentView];
            let container = if document.is_null() { page } else { document };
            let mut ok = true;

            // Status row: a read-only value carrying one of the two localized states.
            let status_text = if ui.finder_menu_status.is_null() {
                String::new()
            } else {
                let value: *mut AnyObject = msg_send![ui.finder_menu_status, stringValue];
                crate::ffi::nsstring_to_rust(value)
            };
            let enabled_text = finder_menu_status_text(true);
            let disabled_text = finder_menu_status_text(false);
            if status_text != enabled_text && status_text != disabled_text {
                log_info!(
                    "[smoke-finder-menu] status row shows {status_text:?}, expected {enabled_text:?} or {disabled_text:?}"
                );
                ok = false;
            }
            if !laid_out(container, ui.finder_menu_status) {
                log_info!("[smoke-finder-menu] status row is not laid out inside the page");
                ok = false;
            }

            let master_state: isize = if ui.finder_menu_enabled.is_null() {
                0
            } else {
                msg_send![ui.finder_menu_enabled, state]
            };
            let master_on = master_state == 1;

            // The master switch.
            if !laid_out(container, ui.finder_menu_enabled) || !row_does_not_overlap(ui.finder_menu_enabled)
            {
                log_info!("[smoke-finder-menu] master switch is missing or collides with its label");
                ok = false;
            }
            // Both rows share the card, so a divider must sit between them.
            let bounds: NSRect = msg_send![container, bounds];
            let content_w = bounds.size.width;
            if !has_separator_between(container, ui.finder_menu_status, ui.finder_menu_enabled, content_w)
            {
                log_info!(
                    "[smoke-finder-menu] the Finder Integration card has no divider between its two rows"
                );
                ok = false;
            }

            // Layout popup with exactly the three advertised choices.
            if ui.finder_menu_layout.is_null() {
                log_info!("[smoke-finder-menu] layout popup is missing");
                ok = false;
            } else {
                // The popup is the app's own select (an NSButton subclass), not an NSPopUpButton:
                // its selection is read through the `index` accessor and its face is the title.
                let labels = [
                    t("settings.finder_menu.layout_flat"),
                    t("settings.finder_menu.layout_grouped"),
                    t("settings.finder_menu.layout_submenu"),
                ];
                let shown = select_value(ui.finder_menu_layout);
                if !labels.iter().any(|label| label == &shown) {
                    log_info!(
                        "[smoke-finder-menu] layout select shows {shown:?}, expected one of {labels:?}"
                    );
                    ok = false;
                }
                if !laid_out(container, ui.finder_menu_layout) {
                    log_info!("[smoke-finder-menu] layout popup is not laid out inside the page");
                    ok = false;
                }
            }

            // One switch per advertised file type.
            if ui.finder_menu_new_file_switches.len() != crate::config::FINDER_MENU_FILE_TYPES.len() {
                log_info!("[smoke-finder-menu] file-type switches do not match the advertised types");
                ok = false;
            }
            for switch in &ui.finder_menu_new_file_switches {
                if !laid_out(container, *switch) {
                    log_info!("[smoke-finder-menu] a file-type switch is not laid out");
                    ok = false;
                }
            }

            // One switch per known terminal; the uninstalled ones are disabled.
            let terminals = crate::finder_menu::terminals();
            if ui.finder_menu_terminal_switches.len() != terminals.len() {
                log_info!("[smoke-finder-menu] terminal switches do not match the terminal catalog");
                ok = false;
            }
            for (spec, switch) in terminals.iter().zip(ui.finder_menu_terminal_switches.iter()) {
                if !laid_out(container, *switch) {
                    log_info!("[smoke-finder-menu] the {} switch is not laid out", spec.name);
                    ok = false;
                    continue;
                }
                // The rows under the master switch are frozen while it is off; when it is on, a
                // terminal row is enabled exactly when the terminal is installed.
                let enabled: bool = msg_send![*switch, isEnabled];
                let installed = crate::finder_menu::terminal_installed(spec.bundle_id);
                let expected = master_on && installed;
                if enabled != expected {
                    log_info!(
                        "[smoke-finder-menu] the {} switch is {} but master_on={master_on} installed={installed}",
                        spec.name,
                        if enabled { "enabled" } else { "disabled" }
                    );
                    ok = false;
                }
            }

            // Copy path.
            if !laid_out(container, ui.finder_menu_copy_path) {
                log_info!("[smoke-finder-menu] copy-path switch is not laid out");
                ok = false;
            }

            // Seed a sentinel so the activation refresh below is provably wired: if the refresh
            // returned early (wrong page, hidden window, unwired call) the sentinel stays.
            widgets::set_field(ui.finder_menu_status, "SENTINEL-STATUS");
            Some(ok)
        })
        .unwrap_or(false);

        // The activation refresh must rewrite the status row from the live extension state.
        window::refresh_finder_menu_status_if_visible();
        let refreshed = with_settings_ui(|ui| {
            ui.as_ref().map(|ui| {
                let value: *mut AnyObject = msg_send![ui.finder_menu_status, stringValue];
                crate::ffi::nsstring_to_rust(value)
            })
        })
        .unwrap_or_default();
        let expected = finder_menu_status_text(crate::finder_menu::extension_enabled());
        if refreshed != expected {
            log_info!(
                "[smoke-finder-menu] the activation refresh did not update the status row: {refreshed:?}, expected {expected:?}"
            );
            ok = false;
        }

        // Negative control: on another page the refresh must leave the row alone.
        select_sidebar(SETTINGS_GENERAL_PAGE_INDEX);
        if let Some(ui) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.finder_menu_status)) {
            if !ui.is_null() {
                widgets::set_field(ui, "SENTINEL-OTHER-PAGE");
                window::refresh_finder_menu_status_if_visible();
                let kept = with_settings_ui(|state| {
                    state.as_ref().map(|state| {
                        let value: *mut AnyObject =
                            msg_send![state.finder_menu_status, stringValue];
                        crate::ffi::nsstring_to_rust(value)
                    })
                })
                .unwrap_or_default();
                if kept != "SENTINEL-OTHER-PAGE" {
                    log_info!(
                        "[smoke-finder-menu] the refresh touched another page's status row: {kept:?}"
                    );
                    ok = false;
                }
            }
        }
        // Leave the page showing its real value for the rest of the run and any screenshot.
        select_sidebar(settings_page_index(SettingsPageId::FinderMenu));
        window::refresh_finder_menu_status_if_visible();

        hide_settings();
        if ok {
            log_info!("[smoke-finder-menu] the Finder Menu page holds");
        } else {
            log_info!("[smoke-finder-menu] the Finder Menu page failed its own assertions");
        }
        ok
    }
}
