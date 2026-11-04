//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.
//!
//! ## What the description can and cannot carry
//!
//! The Mouse page's values are resolved per DEVICE (`resolve_from_config`), not read from the
//! `Config` the description sees alone: the device row's identity is the connected-device list, and
//! the smooth-scrolling sliders are FRACTIONAL values the refill writes with `setDoubleValue`,
//! while a described slider's update is written with `setIntegerValue` (`apply_control_update`).
//! Describing those rows would refill them with a truncated number, so they are drawn by
//! [`MouseHooks`] in the same card sequence. Rows whose value is an exact resolved boolean, or an
//! integer slider, are described here.

use super::page_builder::record_settings_search_options;
use super::*;

/// The page-drawn rows, in document order.
pub(crate) const MOUSE_DEVICE_ROW: &str = "device";
pub(crate) const MOUSE_SCROLL_MODE_ROW: &str = "scroll_mode";
pub(crate) const MOUSE_LINE_COUNT_ROW: &str = "line_count";
pub(crate) const MOUSE_SMOOTH_PRESET_ROW: &str = "smooth_preset";
/// Which of the smooth-scrolling sliders a page-drawn row carries.
///
/// The four are four rows now, so a row's handler binds only its own `SettingsUi` slot instead of
/// writing all four from one block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SmoothSlider {
    Response,
    Speed,
    Acceleration,
    Inertia,
}

/// The four fractional sliders, one row each: the search mirror borrows a row's views and compares
/// each to that row's band, so a single block row would give them a band they do not share.
pub(crate) const MOUSE_SMOOTH_SLIDER_ROWS: [&str; 4] = [
    "smooth_response",
    "smooth_speed",
    "smooth_acceleration",
    "smooth_inertia",
];
pub(crate) const MOUSE_TRACKING_SPEED_ROW: &str = "tracking_speed";

/// The device row's height: it has to cover the longest caption any connected device can produce,
/// because the caption is re-set in place when the popup selection changes. `content_w` is the
/// page's measured content width, the same one the inline builder used.
pub(crate) fn device_row_height(_input: &PageInput, content_w: f64) -> f64 {
    // Measuring a caption reads AppKit text metrics; the page build runs on the main thread.
    unsafe { device_row_height_inner(content_w) }
}

/// The measured height of the device row for a content width (see [`device_row_height`]).
unsafe fn device_row_height_inner(content_w: f64) -> f64 {
    let layout = SettingsLayout::new(content_w);
    let caption_w = layout.control_x - layout.label_x - super::SETTINGS_CONTROL_LABEL_GAP;
    let metrics = SettingsSelect::metrics();
    crate::mouse::device::connected_devices()
        .iter()
        .map(|device| crate::settings::dispatch::device_info_caption(Some(device)))
        .chain(std::iter::once(
            crate::settings::dispatch::device_info_caption(None),
        ))
        .map(|caption| widgets::captioned_row_height(&caption, caption_w))
        .fold(metrics.row_h, f64::max)
}

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

    // ---- Device card: the device row, the scroll mode and the line count are the page's own ----
    w.card("settings.header_mouse_device");
    w.extra_row_measured(MOUSE_DEVICE_ROW, device_row_height);
    w.extra_row(MOUSE_SCROLL_MODE_ROW, SettingsSelect::metrics().row_h);
    w.extra_row_in_group(
        MOUSE_LINE_COUNT_ROW,
        SettingsLayout::SINGLE_LINE_ROW_H,
        Some(RowGroup::LineCount),
    );

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
    w.extra_row_in_group(
        MOUSE_SMOOTH_PRESET_ROW,
        SettingsSelect::metrics().row_h,
        Some(RowGroup::SmoothScrolling),
    );
    // The four sliders are four rows, not one 224pt block: the search mirror borrows a row's views
    // and compares each to that row's band, and a block row would give the labels of its sub-rows a
    // band 168pt tall that they do not share.
    for key in MOUSE_SMOOTH_SLIDER_ROWS {
        w.extra_row_in_group(
            key,
            SettingsLayout::SINGLE_LINE_ROW_H,
            Some(RowGroup::SmoothScrolling),
        );
    }

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
    w.extra_row_in_group(
        MOUSE_TRACKING_SPEED_ROW,
        SettingsLayout::SINGLE_LINE_ROW_H,
        Some(RowGroup::PointerAccel),
    );

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
    w.finish()
}

/// The page hooks the Mouse page owns: the device row, the scroll-mode popup, the line-count
/// slider, the smooth-scrolling preset and its four sliders, the tracking speed, and -- after the
/// cards -- the button-mapping block.
#[allow(dead_code)] // the page build passes the target; the hook hands it to its controls
pub(crate) struct MouseHooks {
    /// The event target the page's controls send their actions to.
    pub(crate) target: *mut AnyObject,
}

unsafe impl Send for MouseHooks {}

impl PageHooks for MouseHooks {
    unsafe fn extra_row(
        &mut self,
        canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        spec: &PageRowSpec,
        origin_y: f64,
        height: f64,
        target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        draw_mouse_row(canvas, ui, spec.key, origin_y, height, target);
        Ok(())
    }

    /// The button-mapping table: a 24pt gap under the mappings card, then the table card the
    /// mapping module fills. It is a block, not a row: the list resizes it as its rows change.
    unsafe fn after_page(
        &mut self,
        canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        ui: &mut SettingsUi,
        target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        draw_mapping_block(canvas, ui, target);
        Ok(())
    }
}

/// A view's own frame, for handing it to the canvas's placement record.
unsafe fn frame_of(view: *mut AnyObject) -> NSRect {
    msg_send![view, frame]
}

/// Draw one of the page's own rows at the origin the canvas advanced to (the row's bottom edge).
unsafe fn draw_mouse_row(
    canvas: &mut PageCanvas,
    ui: &mut SettingsUi,
    key: &str,
    origin_y: f64,
    height: f64,
    target: *mut AnyObject,
) {
    let document = canvas.document();
    let layout = canvas.layout();
    let label_x = layout.label_x;
    let label_w = layout.label_w;
    let ctrl_w = layout.control_w;
    let ctrl_x = layout.control_x;
    let row_h = layout.row_h;
    canvas.set_pending_advance(AdvanceState::Absolute);
    match key {
        MOUSE_DEVICE_ROW => {
            let devices = crate::mouse::device::connected_devices();
            let labels: Vec<String> = devices
                .iter()
                .map(crate::settings::dispatch::device_display_name)
                .collect();
            let refs: Vec<&str> = if labels.is_empty() {
                vec![""]
            } else {
                labels.iter().map(String::as_str).collect()
            };
            let metrics = SettingsSelect::metrics();
            let popup = SettingsControl::popup(
                ctrl_x,
                origin_y + (height - metrics.control_h) / 2.0,
                ctrl_w,
                metrics.control_h,
                &refs,
                0,
            );
            style_flat_popup(popup);
            let _: () = msg_send![popup, setTarget: target];
            let _: () = msg_send![popup, setAction: sel!(handleDeviceChanged:)];
            record_settings_search_row(
                SettingsPageId::Mouse,
                canvas.last_row(),
                &t("settings.header_mouse_device"),
                None,
            );
            let caption = crate::settings::dispatch::device_info_caption(devices.first());
            let caption_w = ctrl_x - label_x - super::SETTINGS_CONTROL_LABEL_GAP;
            let (title, caption_label, control) = SettingsRow::captioned(
                document,
                label_x,
                origin_y,
                caption_w,
                height,
                &t("settings.header_mouse_device"),
                &caption,
                popup,
            );
            for view in [title, caption_label, control] {
                canvas.place_pending_view(view, frame_of(view));
            }
            ui.device_info_caption = caption_label;
            ui.device_indicator = control;
        }
        MOUSE_SCROLL_MODE_ROW => {
            let labels: Vec<String> = SCROLL_MODE_LABEL_KEYS.iter().map(|key| t(key)).collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let metrics = SettingsSelect::metrics();
            let popup = SettingsControl::popup(
                ctrl_x,
                origin_y + (height - metrics.control_h) / 2.0,
                ctrl_w,
                metrics.control_h,
                &refs,
                0,
            );
            style_flat_popup(popup);
            record_settings_search_row(
                SettingsPageId::Mouse,
                canvas.last_row(),
                &t("settings.row_scroll_mode"),
                None,
            );
            record_settings_search_options(&refs);
            let (label, control) = SettingsRow::tall_with_height(
                document,
                label_x,
                origin_y,
                label_w,
                height,
                &t("settings.row_scroll_mode"),
                popup,
            );
            canvas.place_pending_view(label, frame_of(label));
            canvas.place_pending_view(control, frame_of(control));
            bind_control(target, control);
            ui.scroll_mode = control;
        }
        MOUSE_LINE_COUNT_ROW => {
            let slider = SettingsControl::stepped_slider(
                ctrl_x,
                origin_y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                1,
                10,
                1,
                3,
                Some(3.0),
            );
            record_settings_search_row(
                SettingsPageId::Mouse,
                canvas.last_row(),
                &t("settings.row_line_count"),
                None,
            );
            let (label, slider) = SettingsRow::tall(
                document,
                label_x,
                origin_y,
                label_w,
                &t("settings.row_line_count"),
                slider,
            );
            let readout = SettingsRow::attach_slider_readout(document, slider, 3);
            for view in [label, slider, readout] {
                canvas.place_pending_view(view, frame_of(view));
            }
            bind_control(target, slider);
            ui.line_count = slider;
            ui.line_count_label = label;
            ui.line_count_value_label = readout;
        }
        MOUSE_SMOOTH_PRESET_ROW => {
            let labels: Vec<String> = crate::mouse::smooth::presets::SmoothPreset::ALL
                .into_iter()
                .map(|preset| t(preset.label_key()))
                .collect();
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let metrics = SettingsSelect::metrics();
            let popup = SettingsControl::popup(
                ctrl_x,
                origin_y + (height - metrics.control_h) / 2.0,
                ctrl_w,
                metrics.control_h,
                &refs,
                0,
            );
            style_flat_popup(popup);
            record_settings_search_row(
                SettingsPageId::Mouse,
                canvas.last_row(),
                &t("settings.row_mouse_smooth_preset"),
                None,
            );
            record_settings_search_options(&refs);
            let (label, control) = SettingsRow::tall_with_height(
                document,
                label_x,
                origin_y,
                label_w,
                height,
                &t("settings.row_mouse_smooth_preset"),
                popup,
            );
            canvas.place_pending_view(label, frame_of(label));
            canvas.place_pending_view(control, frame_of(control));
            bind_control(target, control);
            ui.smooth_scrolling_preset = control;
        }
        "smooth_response" | "smooth_speed" | "smooth_acceleration" | "smooth_inertia" => {
            let defaults =
                crate::mouse::smooth::presets::SmoothPreset::EaseInOut.default_settings();
            let specs = [
                (
                    "settings.row_mouse_smooth_response",
                    0.0,
                    2.0,
                    defaults.0,
                    SmoothSlider::Response,
                ),
                (
                    "settings.row_mouse_smooth_speed",
                    0.0,
                    8.0,
                    defaults.1,
                    SmoothSlider::Speed,
                ),
                (
                    "settings.row_mouse_smooth_acceleration",
                    0.0,
                    8.0,
                    defaults.2,
                    SmoothSlider::Acceleration,
                ),
                (
                    "settings.row_mouse_smooth_inertia",
                    0.0,
                    8.0,
                    defaults.3,
                    SmoothSlider::Inertia,
                ),
            ];
            let index = MOUSE_SMOOTH_SLIDER_ROWS
                .iter()
                .position(|row| *row == key)
                .unwrap_or(0);
            let (key, minimum, maximum, default_value, which) = specs[index];
            // One slider per row: the row's own band holds it, exactly as the inline builder drew
            // the four (each a `next_row(SINGLE_LINE_ROW_H)`). The four UI pointers are bound from
            // the row index, so each row writes only its own slot.
            let slider = SettingsControl::double_slider(
                ctrl_x,
                origin_y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                minimum,
                maximum,
                default_value,
                Some(default_value),
            );
            record_settings_search_row(SettingsPageId::Mouse, canvas.last_row(), &t(key), None);
            let (label, slider) = SettingsRow::tall_with_height(
                document,
                label_x,
                origin_y,
                label_w,
                SettingsLayout::SINGLE_LINE_ROW_H,
                &t(key),
                slider,
            );
            let readout =
                SettingsRow::attach_slider_readout(document, slider, format!("{default_value:.1}"));
            for view in [label, slider, readout] {
                canvas.place_pending_view(view, frame_of(view));
            }
            bind_control(target, slider);
            match which {
                SmoothSlider::Response => {
                    ui.smooth_scrolling_response = slider;
                    ui.smooth_scrolling_response_value = readout;
                }
                SmoothSlider::Speed => {
                    ui.smooth_scrolling_speed = slider;
                    ui.smooth_scrolling_speed_value = readout;
                }
                SmoothSlider::Acceleration => {
                    ui.smooth_scrolling_acceleration = slider;
                    ui.smooth_scrolling_acceleration_value = readout;
                }
                SmoothSlider::Inertia => {
                    ui.smooth_scrolling_inertia = slider;
                    ui.smooth_scrolling_inertia_value = readout;
                }
            }
        }
        MOUSE_TRACKING_SPEED_ROW => {
            let slider = SettingsControl::double_slider(
                ctrl_x,
                origin_y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                crate::config::MOUSE_ACCELERATION_MIN,
                crate::config::MOUSE_ACCELERATION_MAX,
                crate::mouse::pointer::FALLBACK_ACCELERATION,
                Some(crate::mouse::pointer::FALLBACK_ACCELERATION),
            );
            record_settings_search_row(
                SettingsPageId::Mouse,
                canvas.last_row(),
                &t("settings.row_pointer_tracking_speed"),
                None,
            );
            let (label, slider) = SettingsRow::tall_with_height(
                document,
                label_x,
                origin_y,
                label_w,
                height,
                &t("settings.row_pointer_tracking_speed"),
                slider,
            );
            let readout = SettingsRow::attach_slider_readout(
                document,
                slider,
                pointer_accel_display(crate::mouse::pointer::FALLBACK_ACCELERATION),
            );
            for view in [label, slider, readout] {
                canvas.place_pending_view(view, frame_of(view));
            }
            bind_control(target, slider);
            ui.pointer_accel_label = label;
            ui.pointer_accel_slider = slider;
            ui.pointer_accel_value_label = readout;
        }
        other => log_info!("[settings] Mouse draws no row named {other}"),
    }
}

/// The button-mapping block: the 24pt gap below the mappings card and the table card the mapping
/// module fills. It is a block the canvas owns -- the list resizes it as its rows change -- drawn
/// verbatim from the inline builder.
unsafe fn draw_mapping_block(canvas: &mut PageCanvas, ui: &mut SettingsUi, target: *mut AnyObject) {
    let content_w = canvas.content_width();
    let _ = target;
    let mapping_rows_section = t("settings.section_updates");
    let _ = mapping_rows_section;
    canvas.next_block(24.0);
    let card_w = content_w;
    let card_h = crate::settings::MAPPING_PANEL_TOP
        + (crate::settings::MAPPING_HEADER_H + crate::settings::MAPPING_ROW_H * 3.0)
        + crate::settings::MAPPING_ACTION_TOP
        + crate::settings::MAPPING_ACTION_H
        + crate::settings::MAPPING_CARD_PAD_BOT;
    let card_bottom = canvas.next_block(card_h);
    ui.mapping_layout_row = canvas.last_row();
    // The outer card is a white settings card (same as every other card); only the nested
    // table and the add button carry the gray "dark" treatment from the HTML reference.
    let card_bg: *mut AnyObject = msg_send![class!(NSView), alloc];
    // Align the mapping card with the other settings cards; the nested table keeps its own
    // inset so only the outer border expands to the shared content width.
    let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(0.0, card_bottom), NSSize::new(content_w, card_h))];
    let _: () = msg_send![card_bg, setFlipped: true];
    let _: () = msg_send![card_bg, setAutoresizingMask: 0u64];
    let _: () = msg_send![card_bg, setWantsLayer: true];
    let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
    let _: () = msg_send![bg_layer, setCornerRadius: crate::theme::RADIUS_CARD];
    let _: () = msg_send![bg_layer, setMasksToBounds: true];
    let palette = settings_palette();
    crate::ffi::layer_set_background(bg_layer, crate::ffi::hex_to_cg_color(palette.card_bg));
    crate::ffi::layer_set_border(bg_layer, crate::ffi::hex_to_cg_color(palette.card_border));
    let _: () = msg_send![bg_layer, setBorderWidth: 1.0f64];
    // The nested `.mapping-table`: a rounded, bordered sub-panel behind the rows, giving
    // the bindings the HTML reference's table look.
    let panel: *mut AnyObject = msg_send![class!(NSView), alloc];
    let panel: *mut AnyObject = msg_send![panel, initWithFrame: NSRect::new(NSPoint::new(crate::settings::MAPPING_PANEL_X, crate::settings::MAPPING_PANEL_TOP), NSSize::new(card_w - 2.0 * crate::settings::MAPPING_PANEL_X, crate::settings::MAPPING_HEADER_H + crate::settings::MAPPING_ROW_H * 3.0))];
    let _: () = msg_send![panel, setWantsLayer: true];
    let panel_layer: *mut AnyObject = msg_send![panel, layer];
    let _: () = msg_send![panel_layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let _: () = msg_send![panel_layer, setMasksToBounds: true];
    crate::ffi::layer_set_background(panel_layer, crate::ffi::hex_to_cg_color(palette.field_bg));
    crate::ffi::layer_set_border(
        panel_layer,
        crate::ffi::hex_to_cg_color(palette.card_border),
    );
    let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
    let _: () = msg_send![card_bg, addSubview: panel];
    ui.mapping_panel = panel;
    release_obj(panel);
    // The header band (.mapping-table thead).
    let header_color = settings_text_color(SettingsTextRole::Secondary);
    let header_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION, weight: crate::theme::FONT_WEIGHT_SEMIBOLD];
    for (hx, hw, htext) in [
        (
            crate::settings::MAPPING_PANEL_X + crate::settings::MAPPING_CELL_X,
            120.0,
            t("settings.mapping_column_button"),
        ),
        (
            crate::settings::MAPPING_PANEL_X + crate::settings::MAPPING_CELL_X + 80.0,
            130.0,
            t("settings.mapping_column_action"),
        ),
    ] {
        let hlabel: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let hlabel: *mut AnyObject = msg_send![hlabel, initWithFrame: NSRect::new(NSPoint::new(hx, crate::settings::MAPPING_PANEL_TOP + 7.0), NSSize::new(hw, 18.0))];
        let hns = make_nsstring(&htext);
        let _: () = msg_send![hlabel, setStringValue: hns];
        CFRelease(hns as *const c_void);
        let _: () = msg_send![hlabel, setBezeled: false];
        let _: () = msg_send![hlabel, setDrawsBackground: false];
        let _: () = msg_send![hlabel, setEditable: false];
        let _: () = msg_send![hlabel, setFont: header_font];
        let _: () = msg_send![hlabel, setTextColor: header_color];
        let _: () = msg_send![card_bg, addSubview: hlabel];
        release_obj(hlabel);
    }
    // Hairline under the header band.
    let header_line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let header_line: *mut AnyObject = msg_send![header_line, initWithFrame: NSRect::new(NSPoint::new(crate::settings::MAPPING_PANEL_X + crate::settings::MAPPING_CELL_X, crate::settings::MAPPING_PANEL_TOP + crate::settings::MAPPING_HEADER_H - 1.0), NSSize::new(card_w - 2.0 * (crate::settings::MAPPING_PANEL_X + crate::settings::MAPPING_CELL_X), 1.0))];
    let _: () = msg_send![header_line, setWantsLayer: true];
    let header_line_layer: *mut AnyObject = msg_send![header_line, layer];
    let header_line_color: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
    layer_set_background(header_line_layer, ns_color_to_cg(header_line_color));
    let _: () = msg_send![card_bg, addSubview: header_line];
    release_obj(header_line);
    // Empty-state hint (inside the sub-table when there are no rows).
    let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(NSPoint::new(crate::settings::MAPPING_PANEL_X + crate::settings::MAPPING_CELL_X, crate::settings::MAPPING_PANEL_TOP + crate::settings::MAPPING_HEADER_H + (crate::settings::MAPPING_ROW_H * 3.0) / 2.0 - 9.0), NSSize::new(card_w - 2.0 * (crate::settings::MAPPING_PANEL_X + crate::settings::MAPPING_CELL_X), 18.0))];
    set_field(empty, 0);
    let _: () = msg_send![empty, setBezeled: false];
    let _: () = msg_send![empty, setDrawsBackground: false];
    let _: () = msg_send![empty, setEditable: false];
    let empty_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![empty, setFont: empty_font];
    let _: () = msg_send![empty, setAlignment: 1isize]; // center
    let empty_ns = make_nsstring(&t("settings.mapping_empty"));
    let _: () = msg_send![empty, setStringValue: empty_ns];
    CFRelease(empty_ns as *const c_void);
    let empty_color = settings_text_color(SettingsTextRole::Muted);
    let _: () = msg_send![empty, setTextColor: empty_color];
    let _: () = msg_send![empty, setHidden: true];
    let _: () = msg_send![card_bg, addSubview: empty];
    release_obj(empty);
    ui.mapping_empty = empty;
    // Add-mapping button: full-width action row at the card bottom.
    let add_btn = SettingsButton::action(
        NSRect::new(
            NSPoint::new(
                crate::settings::MAPPING_PANEL_X,
                crate::settings::MAPPING_PANEL_TOP
                    + crate::settings::MAPPING_HEADER_H
                    + crate::settings::MAPPING_ROW_H * 3.0
                    + crate::settings::MAPPING_ACTION_TOP,
            ),
            NSSize::new(
                card_w - 2.0 * crate::settings::MAPPING_PANEL_X,
                crate::settings::MAPPING_ACTION_H,
            ),
        ),
        &t("settings.row_add_mapping"),
        target,
        sel!(handleAddMapping:),
        // A full-width row action on the card surface, not a field: it needs the doc's secondary
        // button surface (and with it a perceptible hover). `Compact`'s field-family wash would leave
        // the hover at 1.02:1 on `card_bg` (see `SettingsButtonRole::ROW_ACTION`).
        SettingsButtonRole::ROW_ACTION,
    );
    let _: () = msg_send![card_bg, addSubview: add_btn];
    release_obj(add_btn);
    ui.add_mapping_button = add_btn;
    let _: () = msg_send![canvas.document(), addSubview: card_bg];
    release_obj(card_bg);
    ui.mapping_card = card_bg;
    ui.mapping_scroll = std::ptr::null_mut();
    ui.mapping_doc = card_bg;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> PageInput {
        PageInput::from_config(Config::default(), None, crate::theme::ThemeInput::current())
    }

    /// The Mouse page is five described rows plus six page-drawn ones: the description carries the
    /// rows whose value is an exact resolved boolean, and the hooks carry the rows whose value is
    /// device-resolved or fractional (a described slider's update is written with setIntegerValue).
    #[test]
    fn the_page_describes_the_boolean_rows_and_declares_the_device_rows_in_place() {
        let view = mouse_page_view(&input());
        assert_eq!(view.page, SettingsPageId::Mouse);
        let described: Vec<&str> = view.rows.iter().map(|row| row.label_key).collect();
        assert_eq!(
            described,
            vec![
                "settings.row_enable_mouse",
                "settings.row_reverse_scroll",
                "settings.row_disable_pointer_accel",
                "settings.row_mapping_enable",
            ],
            "the described rows are the ones whose value is a plain resolved boolean"
        );
        let drawn: Vec<&str> = view.extra_rows.iter().map(|row| row.key).collect();
        assert_eq!(
            drawn,
            vec![
                MOUSE_DEVICE_ROW,
                MOUSE_SCROLL_MODE_ROW,
                MOUSE_LINE_COUNT_ROW,
                MOUSE_SMOOTH_PRESET_ROW,
                MOUSE_SMOOTH_SLIDER_ROWS[0],
                MOUSE_SMOOTH_SLIDER_ROWS[1],
                MOUSE_SMOOTH_SLIDER_ROWS[2],
                MOUSE_SMOOTH_SLIDER_ROWS[3],
                MOUSE_TRACKING_SPEED_ROW,
            ],
            "the page-drawn rows keep their document order"
        );
        // Every described row carries a field identity, and the identities are unique.
        let mut seen = Vec::new();
        for row in &view.rows {
            let field = row.field().expect("a described Mouse row edits a field");
            assert!(!seen.contains(&field), "{field:?} appears twice");
            seen.push(field);
        }
        assert!(description_identity_conflict(&view).is_ok());
    }

    /// The described rows read the resolved per-device view, not raw config: the values are the
    /// device's effective ones, which is what the page has always shown.
    #[test]
    fn the_described_rows_read_the_resolved_device_view() {
        let mut config = Config::default();
        config.mouse.enabled = !config.mouse.enabled;
        let view = mouse_page_view(&PageInput::from_config(
            config.clone(),
            None,
            crate::theme::ThemeInput::current(),
        ));
        let enabled = match view.rows[0].value.clone() {
            Value::Switch(on) => on,
            other => panic!("the master switch is a switch, not {other:?}"),
        };
        assert_eq!(enabled, config.mouse.enabled);
    }

    /// A field change must reach the described rows through the same update description the refill
    /// uses, or an open page keeps showing the old state after a reload.
    #[test]
    fn a_field_change_reaches_the_described_rows() {
        let mut config = Config::default();
        config.mouse.enabled = !config.mouse.enabled;
        let input =
            PageInput::from_config(config.clone(), None, crate::theme::ThemeInput::current());
        let updates = page_update_description(&input, &[SettingsPageId::Mouse]);
        let enabled = updates
            .iter()
            .find(|update| update.field == ControlField::MouseEnabled)
            .expect("the master switch is refilled");
        assert_eq!(
            enabled.value,
            if config.mouse.enabled { "on" } else { "off" }
        );
    }

    /// The measured device row states its height through the layout input, so the renderer advances
    /// by the same number the layout reserved.
    #[test]
    fn the_device_row_measures_its_own_height() {
        let measured = device_row_height(&input(), 600.0);
        assert!(measured > 0.0, "the device row is at least one row tall");
        assert!(
            measured >= SettingsSelect::metrics().row_h,
            "it never shrinks below the select's own row height"
        );
    }

    /// The Mouse page's mode-dependent rows must carry the SAME conditional groups the refresh
    /// functions drive (`update_mode_dependent_visibility` / `update_pointer_accel_visibility`):
    /// a page-drawn row outside its group stays visible when the switch says it should be hidden.
    #[test]
    fn the_mouse_pages_drawn_rows_declare_their_conditional_groups() {
        let view = mouse_page_view(&input());
        let group_of = |key: &str| {
            view.extra_rows
                .iter()
                .find(|extra| extra.key == key)
                .and_then(|extra| extra.group)
        };
        assert_eq!(group_of(MOUSE_LINE_COUNT_ROW), Some(RowGroup::LineCount));
        assert_eq!(
            group_of(MOUSE_SMOOTH_PRESET_ROW),
            Some(RowGroup::SmoothScrolling)
        );
        for key in MOUSE_SMOOTH_SLIDER_ROWS {
            assert_eq!(group_of(key), Some(RowGroup::SmoothScrolling));
        }
        assert_eq!(
            group_of(MOUSE_TRACKING_SPEED_ROW),
            Some(RowGroup::PointerAccel)
        );
    }
}
