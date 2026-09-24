//! Cross-surface UI coordination.
//!
//! This module owns refreshes that affect more than one UI surface. Keeping the orchestration
//! here prevents the settings implementation from reaching into menu and overlay internals.

/// Refresh every visible surface after a theme or locale change.
///
/// The caller must already be on the AppKit main thread. Individual surfaces continue to own
/// their controls and rendering state; this function only coordinates their public refresh APIs.
pub(crate) fn apply_theme_and_locale_refresh() {
    crate::menu::refresh_menu_titles();
    crate::clipboard::refresh_localized_ui();
    unsafe {
        crate::clipboard::apply_theme();
    }
    crate::overlay::apply_theme();
    crate::overlay::refresh_highlight();
    crate::overlay::update_status_label();
}
