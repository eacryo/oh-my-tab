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
    // A locale/theme switch is an independent entry: it samples once for the footer pass it owns.
    crate::overlay::update_status_label(&crate::theme::ThemeInput::current());
    // The footer gear's window is cached across a locale switch: its tooltip and accessibility
    // label would otherwise keep the previous language (same reason the picker's gear is refreshed).
    crate::overlay::refresh_footer_settings_strings();
    // The keystroke panel's surface (frost wash, glass tint, opaque background) is written from the
    // palette, while its keycaps re-read the palette on every render. Without this the surface keeps
    // the previous theme's color and the panel shows light keycaps on a dark shell (or the reverse).
    unsafe {
        crate::keystroke_display::apply_glass_properties();
        // The key panel's own controls are cached with the panel: refresh their tint and strings,
        // or the gear keeps the previous language and palette after a switch.
        crate::keystroke_display::refresh_cached_controls();
    }
    // The guide re-renders its current step with the new palette and re-asserts the
    // window appearance; a no-op when the guide is not on screen.
    crate::onboarding::apply_window_appearance();
    crate::onboarding::rerender_if_visible();
}
