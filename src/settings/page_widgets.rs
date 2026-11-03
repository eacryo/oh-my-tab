//! Views the settings page renderer builds, kept beside the pure description it renders.
//!
//! The renderer places every view in a frame `layout_rows` computed; these helpers only create
//! and style the views, so the row copy keeps the exact face the inline builders used.

use super::*;

/// The title and caption of a stacked row, drawn as two plain labels.
///
/// A captioned row's block carries a title and a measured caption; the renderer draws them itself
/// so they can share the row block the canvas captured, and positions them from the layout's copy
/// box rather than from AppKit's own measurement.
pub(super) unsafe fn stacked_row_labels(
    parent: *mut AnyObject,
    title: &str,
    caption: &str,
) -> (*mut AnyObject, *mut AnyObject) {
    let title_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let title_label: *mut AnyObject = msg_send![title_label, initWithFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(1.0, 1.0),
    )];
    set_field(title_label, title);
    let _: () = msg_send![title_label, setBezeled: false];
    let _: () = msg_send![title_label, setDrawsBackground: false];
    let _: () = msg_send![title_label, setEditable: false];
    let _: () = msg_send![title_label, setSelectable: false];
    let _: () = msg_send![title_label, setUsesSingleLineMode: true];
    let _: () = msg_send![title_label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let title_font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::FONT_CONTROL,
        weight: crate::theme::FONT_WEIGHT_SETTING_LABEL
    ];
    let _: () = msg_send![title_label, setFont: title_font];
    widgets::apply_settings_text_role(title_label, widgets::SettingsTextRole::Primary);
    let _: () = msg_send![title_label, setAutoresizingMask: 4u64];
    let _: () = msg_send![parent, addSubview: title_label];
    release_obj(title_label);

    let caption_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let caption_label: *mut AnyObject = msg_send![caption_label, initWithFrame: NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(1.0, 1.0),
    )];
    set_field(caption_label, caption);
    let _: () = msg_send![caption_label, setBezeled: false];
    let _: () = msg_send![caption_label, setDrawsBackground: false];
    let _: () = msg_send![caption_label, setEditable: false];
    let _: () = msg_send![caption_label, setSelectable: false];
    let _: () = msg_send![caption_label, setUsesSingleLineMode: false];
    let _: () = msg_send![caption_label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    let caption_font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_SETTING_DESC];
    let _: () = msg_send![caption_label, setFont: caption_font];
    // Colour before the line height: setting the line height rebuilds the attributed value from
    // the cell, so a role applied afterwards is silently discarded.
    widgets::apply_settings_text_role(caption_label, widgets::SettingsTextRole::SettingDesc);
    crate::ffi::set_text_field_line_height(
        caption_label,
        crate::theme::line_height(
            crate::theme::FONT_SETTING_DESC,
            crate::theme::LINE_HEIGHT_SETTING_DESC_RATIO,
        ),
    );
    let _: () = msg_send![caption_label, setAutoresizingMask: 4u64];
    let _: () = msg_send![parent, addSubview: caption_label];
    release_obj(caption_label);
    (title_label, caption_label)
}

/// The plain wrapper view a page row's copy is built inside.
///
/// The canvas captures a row as "everything appended since `next_row` returned", and a card is
/// inserted ahead of that list, so a row that owns no control still appends its own title where
/// the inline builders put the label; this wrapper exists so one append can own several views
/// (the title and the caption) at once. The renderer then re-places the wrapper absolutely on
/// every walk, from the copy box the layout computed.
#[allow(dead_code)] // the renderer builds a row's copy inline; the wrapper is kept for the next page
pub(super) unsafe fn row_block(parent: *mut AnyObject, frame: NSRect) -> *mut AnyObject {
    let block: *mut AnyObject = msg_send![class!(NSView), alloc];
    let block: *mut AnyObject = msg_send![block, initWithFrame: frame];
    let _: () = msg_send![block, setAutoresizingMask: 4u64];
    let _: () = msg_send![parent, addSubview: block];
    release_obj(block);
    block
}

/// A plain row's title label, for a row whose control the page places itself.
///
/// Same face as the shared row builder's label (which takes a control to derive its width
/// from); this one is sized by the layout's copy box instead, so a row with a separately
/// placed control still reads exactly like one built inline.
pub(super) unsafe fn plain_row_label(
    parent: *mut AnyObject,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    text: &str,
) -> *mut AnyObject {
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: NSRect::new(
        NSPoint::new(x, y),
        NSSize::new(w.max(1.0), (h - 8.0).max(1.0)),
    )];
    set_field(label, text);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let font: *mut AnyObject = msg_send![
        class!(NSFont),
        systemFontOfSize: crate::theme::FONT_CONTROL,
        weight: crate::theme::FONT_WEIGHT_SETTING_LABEL
    ];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setAlignment: -1isize]; // NSTextAlignmentNatural
    let label_color = widgets::settings_text_color(widgets::SettingsTextRole::Primary);
    let _: () = msg_send![label, setTextColor: label_color];
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 2isize];
    }
    crate::ffi::set_text_field_line_height(
        label,
        crate::theme::line_height(
            crate::theme::FONT_CONTROL,
            crate::theme::LINE_HEIGHT_BODY_RATIO,
        ),
    );
    // Vertical placement belongs to the page layout owner (no MinYMargin).
    let _: () = msg_send![label, setAutoresizingMask: 4u64];
    let _: () = msg_send![parent, addSubview: label];
    release_obj(label);
    label
}
