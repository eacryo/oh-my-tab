//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.
//!
//! ## What the description can and cannot carry
//!
//! The Mouse page's values are resolved per DEVICE (`resolve_selected_from`), not read from the
//! `Config` the description sees: the device row's identity is the connected-device list, and the
//! smooth-scrolling sliders are fractional values that the refill writes with `setDoubleValue`,
//! while a described slider's update is written with `setIntegerValue`. Describing those rows would
//! silently refill them with the wrong number. They are therefore drawn by [`MouseHooks`], in the
//! same card sequence; every row whose value is an exact `Config`/resolved boolean or an integer
//! slider is described here.

use super::*;

/// The page-drawn rows and blocks, in document order.
pub(crate) const MOUSE_DEVICE_ROW: &str = "device";
pub(crate) const MOUSE_SCROLL_MODE_ROW: &str = "scroll_mode";
pub(crate) const MOUSE_LINE_COUNT_ROW: &str = "line_count";
pub(crate) const MOUSE_SMOOTH_PRESET_ROW: &str = "smooth_preset";
pub(crate) const MOUSE_SMOOTH_SLIDERS_ROW: &str = "smooth_sliders";
pub(crate) const MOUSE_TRACKING_SPEED_ROW: &str = "tracking_speed";

/// The Mouse page's description: its rows, their values and their identities.
pub(super) fn mouse_page_view(input: &PageInput) -> PageView {
    let resolved = crate::mouse::resolve::resolve_from_config(&input.config, input.device);
    let mut w = RowWriter::new(SettingsPageId::Mouse, t("settings.sidebar_mouse"));

    // ---- The feature's master switch, in a card of its own ----
    w.in_first_card("settings.header_mouse");
    w.push(
        switch(
            "settings.row_enable_mouse",
            ControlField::MouseEnabled,
            input.config.mouse.enabled,
        )
        .desc("settings.desc_enable_mouse")
        .into_spec(),
    );

    // ---- Device card: the device row and the scroll mode are the page's own (device-resolved);
    // the line count only applies in Line mode, so it is a shared row group's member ----
    w.card("settings.header_mouse_device");
    w.extra_row(MOUSE_DEVICE_ROW, 0.0);
    w.extra_row(MOUSE_SCROLL_MODE_ROW, 0.0);
    w.extra_row(MOUSE_LINE_COUNT_ROW, 0.0);

    // ---- Scrolling: the inverted-scroll switch ----
    w.card("settings.header_mouse_scrolling");
    w.push(
        switch(
            "settings.row_reverse_scroll",
            ControlField::ReverseScroll,
            resolved.reverse_scroll,
        )
        .desc("settings.desc_reverse_scroll")
        .into_spec(),
    );

    // ---- Smooth scrolling: the preset and its four fractional sliders are page-drawn ----
    w.card("settings.header_mouse_smooth_scrolling");
    w.extra_row(MOUSE_SMOOTH_PRESET_ROW, 0.0);
    w.extra_row(MOUSE_SMOOTH_SLIDERS_ROW, 0.0);

    // ---- Pointer: the acceleration switch, then the tracking speed it reveals ----
    w.card("settings.header_mouse_pointer");
    w.push(
        switch(
            "settings.row_disable_pointer_accel",
            ControlField::DisablePointerAccel,
            resolved.disable_acceleration,
        )
        .caption("settings.desc_disable_pointer_accel")
        .into_spec(),
    );
    w.extra_row(MOUSE_TRACKING_SPEED_ROW, 0.0);

    // ---- Button mappings: the master switch, then the page's own table block ----
    w.card("settings.header_mouse_mappings");
    w.push(
        switch(
            "settings.row_mapping_enable",
            ControlField::MappingEnabled,
            resolved.button_mappings_enabled,
        )
        .desc("settings.desc_mapping_enable")
        .into_spec(),
    );
    let mut view = w.finish();
    view.trailing_mapping_block = true;
    view
}
