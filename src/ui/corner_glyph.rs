//! Shared mechanics for the corner glyph buttons -- the switcher footer's gear, the keystroke
//! panel's gear and the clipboard picker's header glyphs.
//!
//! These controls are deliberately siblings in the design system (the corner-glyph-button exception
//! in docs/design-style-en.md sec.10): borderless, no bezel, one faint hover wash, a neutral ink that
//! strengthens under the pointer. Three copies of that recipe drifted, and the drift was visible --
//! two of them painted the raw palette fill where the third applied the mode-tuned wash. The recipe
//! lives here once so "the same family" is a fact rather than a claim.
//!
//! Only the MECHANICS are shared. Each surface keeps its own event policy: the switcher retires its
//! session before opening settings, the keystroke panel has its own armed/AXPress path, and the
//! clipboard header shares its callbacks with the pin and close buttons.

use objc2::runtime::AnyObject;
use std::ffi::c_void;

/// The hover wash for a corner glyph in the given palette.
///
/// The palette's hover fill with its alpha retuned per mode (0x28 dark / 0x18 light): the raw token
/// is calibrated for a card-sized surface, and a 28pt glyph over the panel needs a slightly stronger
/// wash to read at all. Pure, so both modes can be asserted without flipping the global theme and so
/// the three surfaces provably paint one value.
pub(crate) fn hover_wash(palette: crate::theme::UiPalette) -> u32 {
    (palette.hover_bg & 0xFFFF_FF00) | if palette.dark { 0x28 } else { 0x18 }
}

/// The background a corner glyph paints in each state. Normal is fully transparent (the button has
/// no surface of its own until the pointer arrives); hover is [`hover_wash`].
pub(crate) fn glyph_background(palette: crate::theme::UiPalette, hovered: bool) -> u32 {
    if hovered {
        hover_wash(palette)
    } else {
        0x0000_0000
    }
}

/// Apply the corner-glyph tint and wash. The ink is the palette's primary/secondary text routed
/// through the panel-ink resolver, because on a frosted surface the material decides the final
/// colour; the wash is [`glyph_background`].
///
/// Main thread only (AppKit). The caller must not hold a mutex or a RefCell borrow across this: it
/// is called from mouseDown:, whose super call re-enters AppKit's tracking loop.
pub(crate) unsafe fn set_glyph_button_style(
    button: *mut AnyObject,
    palette: crate::theme::UiPalette,
    hovered: bool,
) {
    if button.is_null() {
        return;
    }
    use objc2::{msg_send, sel};
    let _ = sel!(setContentTintColor:);
    let (tint, role) = if hovered {
        (palette.primary_text, crate::glass::PanelInk::Primary)
    } else {
        (palette.secondary_text, crate::glass::PanelInk::Secondary)
    };
    let _: () = msg_send![button, setContentTintColor: crate::glass::panel_ink(tint, role)];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let background = glyph_background(palette, hovered);
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
    }
}

/// Set a button's accessibility label and tooltip from one localized key. Shared so the three
/// surfaces cannot describe the same control differently.
pub(crate) unsafe fn refresh_glyph_button_strings(button: *mut AnyObject, key: &str) {
    if button.is_null() {
        return;
    }
    use objc2::msg_send;
    let text = crate::i18n::t(key);
    let label = crate::ffi::make_nsstring(&text);
    let _: () = msg_send![button, setAccessibilityLabel: label];
    crate::ffi::CFRelease(label as *const c_void);
    let tooltip = crate::ffi::make_nsstring(&text);
    let _: () = msg_send![button, setToolTip: tooltip];
    crate::ffi::CFRelease(tooltip as *const c_void);
}

/// Call NSButton's own mouseDown:, which runs the highlight/press tracking loop and ultimately
/// dispatches the action.
///
/// Overriding mouseDown: without this replaces that loop: the button still looks right (the
/// highlight comes from the style setter) but never tracks the press, so its action never fires.
/// That was a shipped defect in both new gears, and the reason a shared helper matters.
pub(crate) unsafe fn call_super_mouse_down(
    receiver: *mut c_void,
    defined: crate::ffi::StaticClass,
    cmd: objc2::runtime::Sel,
    event: *mut c_void,
) {
    type MouseDown =
        unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, objc2::runtime::Sel, *mut c_void);
    let mut sup = crate::ffi::ObjcSuper::for_defined_class(receiver, defined);
    let call: MouseDown = std::mem::transmute(objc2::ffi::objc_msgSendSuper as *const ());
    call(&mut sup, cmd, event);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wash keeps the palette's RGB and retunes only the alpha, per mode. This is the value the
    /// corner-glyph read-back assertion compares a live layer against, so it must be the wash and
    /// not the raw fill.
    #[test]
    fn the_shared_wash_keeps_the_palette_rgb_and_tunes_alpha_by_mode() {
        for dark in [false, true] {
            let palette = crate::theme::ui_palette_for_mode(dark);
            let wash = hover_wash(palette);
            assert_eq!(wash & 0xFFFF_FF00, palette.hover_bg & 0xFFFF_FF00);
            assert_eq!(wash & 0xFF, if dark { 0x28 } else { 0x18 });
            // The counter-example: if this ever collapses to the raw token, the three surfaces can
            // drift apart again without this test noticing.
            assert_ne!(
                wash, palette.hover_bg,
                "the corner-glyph wash is the raw palette fill, so the mode tuning is gone"
            );
        }
    }

    /// Normal is transparent and hover is the wash: the two states a layer read-back distinguishes.
    #[test]
    fn glyph_background_is_transparent_until_hovered() {
        for dark in [false, true] {
            let palette = crate::theme::ui_palette_for_mode(dark);
            assert_eq!(glyph_background(palette, false), 0x0000_0000);
            assert_eq!(glyph_background(palette, true), hover_wash(palette));
            assert_ne!(
                glyph_background(palette, true),
                glyph_background(palette, false)
            );
        }
    }
}
