//! The switcher overlay's footer settings control.
//!
//! The gear sits in the footer band's trailing corner and opens the settings window on the App
//! Switcher page. It is the same control family as the clipboard picker's corner gear: a borderless
//! glyph button with normal/hover/pressed only (no keyboard focus ring), because the overlay is a
//! nonactivating panel whose keyboard focus belongs to the card container, and the control has no
//! selected or open state to express. See the corner-glyph-button exception in
//! docs/design-style-en.md section 10.
//!
//! Why the click retires the session before showing settings:
//!
//! show_settings_page activates the app and orders the settings window front, which can
//! synchronously resign the overlay panel's key status and re-enter the dismiss path. Doing that
//! while this session is still live would let the resign handler hide the panel mid-click, or let a
//! trigger release commit the highlighted card on the way out. So the click ends the session
//! LOGICALLY first, while holding no borrow, and only then presents settings and performs the
//! visual hide. The trigger release that may follow finds state.visible == false, and is a no-op.

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use super::*;
use crate::ffi::{make_nsstring, release_obj, CFRelease};
use crate::ffi::{MainThreadSlot, StaticClass};

/// The footer gear button (kept for theme repaints and layout, like the status label).
pub(crate) static FOOTER_SETTINGS_BUTTON: MainThreadSlot<Option<ObjPtr>> =
    MainThreadSlot::new(None);

/// Whether a press on the gear is in progress. The click owns the gesture from mouseDown to mouseUp:
/// window input must stay enabled while it runs, and every session-ending path clears it.
static ARMED: AtomicBool = AtomicBool::new(false);

pub(crate) fn arm() {
    ARMED.store(true, Ordering::SeqCst);
}

/// Disarm without acting: a press released outside the button, or any session-ending path.
pub(crate) fn disarm() {
    ARMED.store(false, Ordering::SeqCst);
}

pub(crate) fn is_armed() -> bool {
    ARMED.load(Ordering::SeqCst)
}

/// The button class: a borderless glyph button with the neutral hover the corner-glyph exception
/// describes. Deliberately NOT the card close button's class -- that one hardcodes the destructive
/// red hover, which would misread as a dangerous action on a settings control.
///
/// Returned as the class handle rather than `*mut AnyObject`, so a super dispatch can name the
/// class that defined the override.
fn footer_settings_button_class_handle() -> StaticClass {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabFooterSettingsButton", || {
        crate::ffi::build_class(
            c"OhMyTabFooterSettingsButton",
            class!(NSButton),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(mouseEntered:),
                    footer_settings_entered
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(mouseExited:),
                    footer_settings_exited
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                // The press arms the gesture; the action (dispatched on mouseUp) consumes it. Arming at
                // press is what lets a release outside the button cancel cleanly instead of leaving the
                // session half-ended.
                builder.add_method::<AnyObject, _>(
                    sel!(mouseDown:),
                    footer_settings_mouse_down
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
}

extern "C" fn footer_settings_mouse_down(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    crate::callback_guard::void("footer_settings_mouse_down", || unsafe {
        arm();
        set_footer_settings_style(_self, true);
        // NSButton's own mouseDown: runs the highlight/press tracking loop without which the button
        // never tracks the press and never dispatches its action.
        // The defined class, not the instance: `_self` is an OhMyTabFooterSettingsButton
        // instance, and passing it as the class would make objc_super start its search at
        // something that is not a class at all.
        crate::ui::corner_glyph::call_super_mouse_down(
            _self as *mut c_void,
            footer_settings_button_class_handle(),
            sel!(mouseDown:),
            event as *mut c_void,
        );
    });
}

extern "C" fn footer_settings_entered(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("footer_settings_entered", || unsafe {
        set_footer_settings_style(_self, true);
    });
}

extern "C" fn footer_settings_exited(_self: *mut AnyObject, _cmd: Sel, _event: *mut AnyObject) {
    crate::callback_guard::void("footer_settings_exited", || unsafe {
        // Leaving the button during the press cancels the gesture: the action will still be sent on
        // mouseUp (AppKit tracks the press), so the arm flag is what tells it apart from a real click.
        disarm();
        set_footer_settings_style(_self, false);
    });
}

/// Normal/hover/pressed for the gear: a neutral ink that strengthens under the pointer over one
/// faint wash. It never borrows the close button's destructive red -- that colour says "this
/// destroys something", and opening settings destroys nothing.
pub(crate) unsafe fn set_footer_settings_style(button: *mut AnyObject, hovered: bool) {
    // The shared corner-glyph recipe, so this gear paints the same wash as the picker and the
    // keystroke panel. It used the raw hover token, which omits the per-mode alpha tuning.
    crate::ui::corner_glyph::set_glyph_button_style(button, ui_palette(), hovered);
}

/// The gear's frame inside the panel: the trailing corner of the footer band, the corner-safe inset
/// in from the trailing edge and vertically centred in the band.
pub(crate) fn footer_settings_frame(panel_size: NSSize) -> NSRect {
    let band = crate::theme::footer_band_h();
    let radius = CONFIG.read().unwrap().appearance.corner_radius;
    let inset = crate::theme::footer_control_inset(
        radius,
        panel_size.width,
        panel_size.height,
        crate::theme::FOOTER_CONTROL_SIZE,
        band,
    )
    .max(crate::theme::H_PADDING);
    let size = crate::theme::FOOTER_CONTROL_SIZE;
    NSRect::new(
        NSPoint::new(
            (panel_size.width - inset - size).max(0.0),
            ((band - size) / 2.0).max(0.0),
        ),
        NSSize::new(size, size),
    )
}

/// Build the gear button. Returns a +1 reference; the caller adds it to a superview and releases it.
pub(crate) unsafe fn make_footer_settings_button(frame: NSRect) -> *mut AnyObject {
    let button: *mut AnyObject = msg_send![
        footer_settings_button_class_handle().0 as *mut AnyObject,
        alloc
    ];
    let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
    let _: () = msg_send![button, setBordered: false];
    let _: () = msg_send![button, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    // An SF Symbol sized by its own configuration: a symbol image is not sized by the button's font.
    let symbol_name = make_nsstring("gearshape");
    let image: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_name,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_name as *const c_void);
    if !image.is_null() {
        let config: *mut AnyObject = msg_send![
            class!(NSImageSymbolConfiguration),
            configurationWithPointSize: crate::theme::FONT_CAPTION,
            weight: crate::theme::FONT_WEIGHT_REGULAR
        ];
        let image: *mut AnyObject = msg_send![image, imageWithSymbolConfiguration: config];
        let _: () = msg_send![image, setTemplate: true];
        let _: () = msg_send![button, setImage: image];
    }
    let empty = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty];
    CFRelease(empty as *const c_void);
    let _: () = msg_send![button, setTarget: super::cancel::overlay_observer_target()];
    let _: () = msg_send![button, setAction: sel!(openSwitcherSettings:)];
    set_footer_settings_style(button, false);
    let opts: u64 = 0x01 | 0x80; // NSTrackingMouseEnteredAndExited | ActiveAlways
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        options: opts,
        owner: button,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![button, addTrackingArea: tracking];
    release_obj(tracking);
    let label = make_nsstring(&crate::i18n::t("overlay.open_settings"));
    let _: () = msg_send![button, setAccessibilityLabel: label];
    CFRelease(label as *const c_void);
    let tooltip = make_nsstring(&crate::i18n::t("overlay.open_settings"));
    let _: () = msg_send![button, setToolTip: tooltip];
    CFRelease(tooltip as *const c_void);
    button
}

/// Refresh the gear's localized strings after a locale change: the window is cached across a locale
/// switch, so its tooltip and accessibility label would otherwise keep the previous language.
pub(crate) fn refresh_localized_strings() {
    let Some(button) = *FOOTER_SETTINGS_BUTTON.lock().unwrap() else {
        return;
    };
    unsafe {
        crate::ui::corner_glyph::refresh_glyph_button_strings(button.0, "overlay.open_settings");
    }
}

/// Reposition the gear after the panel resizes and repaint it from the current palette.
pub(crate) fn layout_and_repaint(panel_size: NSSize) {
    let Some(button) = *FOOTER_SETTINGS_BUTTON.lock().unwrap() else {
        return;
    };
    unsafe {
        let _: () = msg_send![button.0, setFrame: footer_settings_frame(panel_size)];
        set_footer_settings_style(button.0, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recompute the same corner-safe inset the frame uses, from the live config radius.
    fn expected_inset(panel_size: NSSize) -> f64 {
        let radius = crate::config::CONFIG
            .read()
            .unwrap()
            .appearance
            .corner_radius;
        crate::theme::footer_control_inset(
            radius,
            panel_size.width,
            panel_size.height,
            crate::theme::FOOTER_CONTROL_SIZE,
            crate::theme::footer_band_h(),
        )
        .max(crate::theme::H_PADDING)
    }

    #[test]
    fn footer_settings_frame_sits_in_the_footer_band_trailing_corner() {
        let panel = NSSize::new(600.0, 400.0);
        let rect = footer_settings_frame(panel);
        let size = crate::theme::FOOTER_CONTROL_SIZE;
        let band = crate::theme::footer_band_h();
        let inset = expected_inset(panel);

        assert_eq!(rect.size.width, size);
        assert_eq!(rect.size.height, size);
        assert_eq!(rect.origin.y, ((band - size) / 2.0).max(0.0));
        assert_eq!(rect.origin.x, (panel.width - inset - size).max(0.0));

        // A normal panel keeps the whole control inside, inset from the trailing edge.
        assert!(rect.origin.x >= 0.0);
        assert!(rect.origin.y >= 0.0);
        assert!(rect.origin.x + rect.size.width <= panel.width);
        assert!(rect.origin.y + rect.size.height <= panel.height);
        assert_eq!(panel.width - (rect.origin.x + rect.size.width), inset);
    }

    #[test]
    fn footer_settings_frame_clamps_x_for_a_tiny_panel() {
        // Narrower than the inset plus the control, so the trailing layout would go negative.
        let panel = NSSize::new(10.0, 400.0);
        let rect = footer_settings_frame(panel);
        assert_eq!(rect.origin.x, 0.0);
        assert_eq!(rect.size.width, crate::theme::FOOTER_CONTROL_SIZE);
    }

    #[test]
    fn arming_round_trips_and_ends_disarmed() {
        // Process-global flag: end disarmed so no later test observes it armed.
        disarm();
        assert!(!is_armed());
        arm();
        assert!(is_armed());
        disarm();
        assert!(!is_armed());
    }
}
