//! Custom settings select: state, popup panel, keyboard interaction, and option layout.

use super::widgets::settings_palette;
use super::*;

struct SettingsSelectState {
    items: Vec<String>,
    item_symbols: Vec<Option<String>>,
    selected: isize,
    /// The options panel view (non-zero while open).
    panel: usize,
    /// The floating window hosting the options panel (non-zero while open; see
    /// settings_select_open).
    popup: usize,
    open: bool,
}

fn settings_select_needs_wrap(
    natural_width: f64,
    available_width: f64,
    has_explicit_newline: bool,
) -> bool {
    has_explicit_newline
        || (natural_width.is_finite() && natural_width > available_width.max(1.0) + 0.5)
}

fn settings_select_centered_text_geometry(control_height: f64, measured_height: f64) -> (f64, f64) {
    let control_height = control_height.max(1.0);
    let text_height = if measured_height.is_finite() {
        measured_height.max(1.0).min(control_height)
    } else {
        control_height
    };
    ((control_height - text_height).max(0.0) / 2.0, text_height)
}

static SETTINGS_SELECT_STATES: LazyLock<Mutex<HashMap<usize, SettingsSelectState>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SETTINGS_SELECT_LABEL_VIEWS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SETTINGS_SELECT_ARROW_VIEWS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static SETTINGS_SELECT_ITEM_LABEL_VIEWS: LazyLock<Mutex<HashMap<usize, usize>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ACTIVE_SETTINGS_SELECT: Mutex<Option<usize>> = Mutex::new(None);

struct SettingsSelectClass(*mut AnyObject);
unsafe impl Send for SettingsSelectClass {}
unsafe impl Sync for SettingsSelectClass {}

static SETTINGS_SELECT_CLASS: OnceLock<SettingsSelectClass> = OnceLock::new();

struct SettingsSelectItemClass(*mut AnyObject);
unsafe impl Send for SettingsSelectItemClass {}
unsafe impl Sync for SettingsSelectItemClass {}

static SETTINGS_SELECT_ITEM_CLASS: OnceLock<SettingsSelectItemClass> = OnceLock::new();

/// Clear runtime state before the settings window and its controls are destroyed.
pub(super) fn clear_settings_select_registry() {
    // Close any popup windows first: once the registry is cleared they can never be found again and
    // would linger on screen as orphans.
    let popups: Vec<usize> = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .values()
        .filter(|state| state.popup != 0)
        .map(|state| state.popup)
        .collect();
    unsafe {
        for popup in popups {
            close_select_popup(popup as *mut AnyObject);
        }
    }
    unsafe { remove_select_monitor() };
    SETTINGS_SELECT_STATES.lock().unwrap().clear();
    SETTINGS_SELECT_LABEL_VIEWS.lock().unwrap().clear();
    SETTINGS_SELECT_ARROW_VIEWS.lock().unwrap().clear();
    SETTINGS_SELECT_ITEM_LABEL_VIEWS.lock().unwrap().clear();
    *ACTIVE_SETTINGS_SELECT.lock().unwrap() = None;
}

unsafe fn settings_select_set_title(button: *mut AnyObject, title: &str) {
    // Render the value in a dedicated label so the trailing arrow always has its own reserved
    // column. A native NSButton title has no reliable content width once a child arrow is added,
    // so long values can otherwise paint underneath the arrow.
    let bounds: NSRect = msg_send![button, bounds];
    let label_frame = NSRect::new(
        // Use the complete control height: the cell centers single-line values, while the
        // remaining height is available for a wrapped second line.
        NSPoint::new(12.0, 0.0),
        NSSize::new(
            (bounds.size.width - 12.0 - 16.0 - 8.0 - 12.0).max(1.0),
            bounds.size.height.max(1.0),
        ),
    );
    let existing_label = SETTINGS_SELECT_LABEL_VIEWS
        .lock()
        .unwrap()
        .get(&(button as usize))
        .copied()
        .unwrap_or(0) as *mut AnyObject;
    let label = if existing_label.is_null() {
        let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let label: *mut AnyObject = msg_send![label, initWithFrame: label_frame];
        let _: () = msg_send![label, setBezeled: false];
        let _: () = msg_send![label, setDrawsBackground: false];
        let _: () = msg_send![label, setEditable: false];
        let _: () = msg_send![label, setSelectable: false];
        let _: () = msg_send![label, setAlignment: 0isize]; // NSTextAlignmentLeft
        let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
            let _: () = msg_send![label, setMaximumNumberOfLines: 0isize];
        }
        let _: () = msg_send![label, setAutoresizingMask: 2u64]; // width sizable
        let _: () = msg_send![button, addSubview: label];
        SETTINGS_SELECT_LABEL_VIEWS
            .lock()
            .unwrap()
            .insert(button as usize, label as usize);
        release_obj(label);
        label
    } else {
        existing_label
    };
    let _: () = msg_send![label, setFrame: label_frame];
    let title_ns = make_nsstring(title);
    let _: () = msg_send![label, setStringValue: title_ns];
    CFRelease(title_ns as *const c_void);
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setPreferredMaxLayoutWidth: label_frame.size.width];

    // AppKit's single-line cell has the correct baseline, while its multi-line cell is top-biased.
    // Use the cell's unconstrained natural width to decide whether wrapping is needed; fittingSize
    // is already constrained by preferredMaxLayoutWidth and cannot make that decision reliably.
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 1isize];
    }
    let cell: *mut AnyObject = msg_send![label, cell];
    let natural_size = if cell.is_null() {
        NSSize::new(0.0, 0.0)
    } else {
        msg_send![cell, cellSize]
    };
    let needs_wrap = settings_select_needs_wrap(
        natural_size.width,
        label_frame.size.width,
        title.contains('\n'),
    );

    if !needs_wrap {
        // A fixed baseline alone does not center a child NSTextField whose frame spans the whole
        // button. Shrink the field to the natural line height and center that frame explicitly.
        let mut centered_frame = label_frame;
        let (text_y, text_height) =
            settings_select_centered_text_geometry(bounds.size.height, natural_size.height);
        centered_frame.origin.y = text_y;
        centered_frame.size.height = text_height;
        let _: () = msg_send![label, setFrame: centered_frame];
    } else {
        let _: () = msg_send![label, setUsesSingleLineMode: false];
        if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
            let _: () = msg_send![label, setMaximumNumberOfLines: 0isize];
        }
        let wrapped_size: NSSize = msg_send![
            label,
            sizeThatFits: NSSize::new(label_frame.size.width, 10_000.0)
        ];
        // Fit the multi-line label to its measured height so its text block is centered as a unit.
        let mut centered_frame = label_frame;
        let (text_y, text_height) = settings_select_centered_text_geometry(
            bounds.size.height,
            wrapped_size.height.max(natural_size.height),
        );
        centered_frame.origin.y = text_y;
        centered_frame.size.height = text_height;
        let _: () = msg_send![label, setFrame: centered_frame];
    }
    let empty_title = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);
    // The native title is kept empty; its visible value is the wrapped label above.
}

unsafe fn settings_select_set_label_color(button: *mut AnyObject, color: *mut AnyObject) {
    let label = SETTINGS_SELECT_LABEL_VIEWS
        .lock()
        .unwrap()
        .get(&(button as usize))
        .copied()
        .unwrap_or(0) as *mut AnyObject;
    if !label.is_null() {
        let _: () = msg_send![label, setTextColor: color];
    }
}

/// Select surfaces deliberately use opaque colors; only the surrounding settings window remains
fn settings_select_surface_color(palette: UiPalette) -> u32 {
    if palette.dark {
        0x151515FF
    } else {
        0xFCFCFCFF
    }
}

fn settings_select_item_active_color(palette: UiPalette) -> u32 {
    if palette.dark {
        0x1C1C1CFF
    } else {
        0xF5F5F5FF
    }
}

/// Update the trigger surface and arrow without changing the selected value.
unsafe fn settings_select_apply_visual(button: *mut AnyObject, open: bool) {
    let (title, enabled) = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&(button as usize))
        .map(|state| {
            let title = state
                .items
                .get(state.selected.max(0) as usize)
                .cloned()
                .unwrap_or_default();
            (title, msg_send![button, isEnabled])
        })
        .unwrap_or_else(|| (String::new(), msg_send![button, isEnabled]));
    settings_select_set_title(button, &title);

    // Keep one downward chevron and rotate its layer so opening/closing is continuous.
    let symbol = "chevron.down";
    // Keep the arrow outside NSButton's title/image layout. AppKit otherwise lets the
    // symbol's intrinsic size affect the button's layout, which can make the arrow huge
    let bounds: NSRect = msg_send![button, bounds];
    let arrow_size = 16.0;
    let arrow_frame = NSRect::new(
        NSPoint::new(
            bounds.origin.x + (bounds.size.width - arrow_size - 12.0).max(0.0),
            bounds.origin.y + (bounds.size.height - arrow_size).max(0.0) / 2.0,
        ),
        NSSize::new(arrow_size, arrow_size),
    );
    let existing_arrow_view = SETTINGS_SELECT_ARROW_VIEWS
        .lock()
        .unwrap()
        .get(&(button as usize))
        .copied()
        .unwrap_or(0) as *mut AnyObject;
    let arrow_view = if existing_arrow_view.is_null() {
        let view = make_symbol_image_view(symbol, arrow_frame);
        let _: () = msg_send![button, addSubview: view];
        SETTINGS_SELECT_ARROW_VIEWS
            .lock()
            .unwrap()
            .insert(button as usize, view as usize);
        release_obj(view);
        view
    } else {
        existing_arrow_view
    };
    let _: () = msg_send![arrow_view, setFrame: arrow_frame];
    let image = make_symbol_image(symbol, NSSize::new(arrow_size, arrow_size));
    if !image.is_null() {
        let _: () = msg_send![arrow_view, setImage: image];
    }

    // Rotate a dedicated layer around the icon center. Without an explicit layer-backed view
    // and anchor point, AppKit can apply the transform in the parent button's coordinate space.
    let _: () = msg_send![arrow_view, setWantsLayer: true];
    let arrow_layer: *mut AnyObject = msg_send![arrow_view, layer];
    if !arrow_layer.is_null() {
        let _: () = msg_send![arrow_layer, setAnchorPoint: NSPoint::new(0.5, 0.5)];
        let _: () = msg_send![
            arrow_layer,
            setPosition: NSPoint::new(
                arrow_frame.origin.x + arrow_frame.size.width / 2.0,
                arrow_frame.origin.y + arrow_frame.size.height / 2.0,
            )
        ];
        let target_angle = if open { std::f64::consts::PI } else { 0.0 };
        let key_path = make_nsstring("transform.rotation.z");
        let target_value: *mut AnyObject =
            msg_send![class!(NSNumber), numberWithDouble: target_angle];
        let _: () = msg_send![arrow_layer, setValue: target_value, forKeyPath: key_path];

        if !existing_arrow_view.is_null() {
            let presentation: *mut AnyObject = msg_send![arrow_layer, presentationLayer];
            let from_angle = if presentation.is_null() {
                if open {
                    0.0
                } else {
                    std::f64::consts::PI
                }
            } else {
                let value: *mut AnyObject = msg_send![presentation, valueForKeyPath: key_path];
                if value.is_null() {
                    if open {
                        0.0
                    } else {
                        std::f64::consts::PI
                    }
                } else {
                    msg_send![value, doubleValue]
                }
            };
            let from_value: *mut AnyObject =
                msg_send![class!(NSNumber), numberWithDouble: from_angle];
            let animation: *mut AnyObject = msg_send![
                class!(CASpringAnimation),
                animationWithKeyPath: key_path
            ];
            let _: () = msg_send![animation, setFromValue: from_value];
            let _: () = msg_send![animation, setToValue: target_value];
            let _: () = msg_send![animation, setMass: 1.0f64];
            let _: () = msg_send![animation, setStiffness: 300.0f64];
            let _: () = msg_send![animation, setDamping: 25.0f64];
            let _: () = msg_send![animation, setInitialVelocity: 0.0f64];
            let duration: f64 = msg_send![animation, settlingDuration];
            let _: () = msg_send![animation, setDuration: duration.max(0.32)];
            let animation_key = make_nsstring("settings-select-arrow-rotation");
            let _: () = msg_send![arrow_layer, addAnimation: animation, forKey: animation_key];
            CFRelease(animation_key as *const c_void);
        }
        CFRelease(key_path as *const c_void);
    }

    let palette = settings_palette();
    let tint = crate::ffi::hex_to_ns_color(if enabled {
        palette.primary_text
    } else {
        palette.muted_text
    });
    let _: () = msg_send![button, setContentTintColor: tint];
    settings_select_set_label_color(button, tint);
    let _: () = msg_send![arrow_view, setContentTintColor: tint];
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        let background = settings_select_surface_color(palette);
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![layer, setCornerRadius: 12.0f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
}

/// The option rows inside a panel. They normally sit directly on the panel; when the list does not
/// fit they are nested one level deeper inside a scroll view (see settings_select_open), so the
/// cleanup paths (cancelling reveal animations, dropping label registrations) must descend that
/// level or they miss every row.
unsafe fn settings_select_item_views(panel: *mut AnyObject) -> Vec<*mut AnyObject> {
    let mut out = Vec::new();
    if panel.is_null() {
        return out;
    }
    let subviews: *mut AnyObject = msg_send![panel, subviews];
    if subviews.is_null() {
        return out;
    }
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: *mut AnyObject = msg_send![subviews, objectAtIndex: index];
        if view.is_null() {
            continue;
        }
        let is_scroll: bool = msg_send![view, isKindOfClass: class!(NSScrollView)];
        if !is_scroll {
            out.push(view);
            continue;
        }
        let document: *mut AnyObject = msg_send![view, documentView];
        if document.is_null() {
            continue;
        }
        let inner: *mut AnyObject = msg_send![document, subviews];
        if inner.is_null() {
            continue;
        }
        let inner_count: usize = msg_send![inner, count];
        for inner_index in 0..inner_count {
            let item: *mut AnyObject = msg_send![inner, objectAtIndex: inner_index];
            if !item.is_null() {
                out.push(item);
            }
        }
    }
    out
}

unsafe fn settings_select_cancel_item_reveals(panel: *mut AnyObject) {
    for item in settings_select_item_views(panel) {
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: item,
            selector: sel!(reveal),
            object: std::ptr::null::<AnyObject>()
        ];
    }
}

unsafe fn settings_select_close(button: *mut AnyObject) {
    let panel = {
        let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
        let Some(state) = states.get_mut(&(button as usize)) else {
            return;
        };
        state.open = false;
        state.panel as *mut AnyObject
    };
    if !panel.is_null() {
        settings_select_cancel_item_reveals(panel);
        let panel_layer: *mut AnyObject = msg_send![panel, layer];
        if !panel_layer.is_null() {
            let presentation: *mut AnyObject = msg_send![panel_layer, presentationLayer];
            // CALayer.opacity is a CGFloat on macOS, which is f32 in this objc2 ABI.
            let from_opacity: f32 = if presentation.is_null() {
                msg_send![panel_layer, opacity]
            } else {
                msg_send![presentation, opacity]
            };
            let open_key = make_nsstring("settings-select-open");
            let _: () = msg_send![panel_layer, removeAnimationForKey: open_key];
            CFRelease(open_key as *const c_void);
            // Commit the hidden end state to the model layer before adding the fade. This avoids
            // a one-frame return to opacity 1 when Core Animation removes the animation.
            let _: () = msg_send![panel_layer, setOpacity: 0.0f32];
            let key_path = make_nsstring("opacity");
            let animation: *mut AnyObject = msg_send![
                class!(CABasicAnimation),
                animationWithKeyPath: key_path
            ];
            CFRelease(key_path as *const c_void);
            let from: *mut AnyObject =
                msg_send![class!(NSNumber), numberWithFloat: from_opacity.clamp(0.0, 1.0)];
            let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithFloat: 0.0f32];
            let _: () = msg_send![animation, setFromValue: from];
            let _: () = msg_send![animation, setToValue: to];
            let _: () = msg_send![animation, setDuration: 0.16f64];
            let animation_key = make_nsstring("settings-select-close");
            let _: () = msg_send![panel_layer, addAnimation: animation, forKey: animation_key];
            CFRelease(animation_key as *const c_void);
        }
        let _: () = msg_send![
            button,
            performSelector: sel!(finishClose:),
            withObject: panel,
            afterDelay: 0.16f64
        ];
    }
    if ACTIVE_SETTINGS_SELECT
        .lock()
        .unwrap()
        .is_some_and(|active| active == button as usize)
    {
        *ACTIVE_SETTINGS_SELECT.lock().unwrap() = None;
    }
    settings_select_apply_visual(button, false);
}

extern "C" fn settings_select_finish_close(this: *mut c_void, _cmd: Sel, panel: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let panel = panel as *mut AnyObject;
        let (should_remove, popup) = {
            let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
            match states.get_mut(&(button as usize)) {
                Some(state) if !state.open && state.panel == panel as usize => {
                    state.panel = 0;
                    let popup = state.popup as *mut AnyObject;
                    state.popup = 0;
                    (true, popup)
                }
                _ => (false, std::ptr::null_mut()),
            }
        };
        if should_remove && !panel.is_null() {
            settings_select_remove_item_labels(panel);
            let _: () = msg_send![panel, removeFromSuperview];
        }
        // Detaching the view is not enough: the popup window itself must be closed and released too
        // (no view retains it; we hold its alloc +1).
        if should_remove {
            close_select_popup(popup);
            // The monitor only serves the open dropdown: drop it once nothing is open.
            if ACTIVE_SETTINGS_SELECT.lock().unwrap().is_none() {
                remove_select_monitor();
            }
        }
    }
}

/// A local event monitor installed while a dropdown is open: a click outside the popup panel and
/// the trigger closes it. Only the settings window's `sendEvent` used to close the dropdown; once
/// the dropdown moved into its own popup window, clicks in other windows (the recording panel) no
/// longer passed through there, so it stopped closing. The local monitor sees every mouse-down in
/// this app before dispatch, which is the only place that covers every window (a global monitor
/// would need extra permissions and we do not need cross-app coverage).
struct SelectMonitor {
    monitor: *mut AnyObject,
    /// The monitor borrows the block pointer rather than retaining it, so we keep it alive here.
    #[allow(dead_code)]
    handler: block2::RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject>,
}

unsafe impl Send for SelectMonitor {}
unsafe impl Sync for SelectMonitor {}

static SELECT_MONITOR: Mutex<Option<SelectMonitor>> = Mutex::new(None);

/// Install the click-outside monitor (no-op when already installed; only one dropdown is open at a
/// time).
unsafe fn install_select_monitor() {
    if SELECT_MONITOR.lock().unwrap().is_some() {
        return;
    }
    let handler: block2::RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject> =
        block2::RcBlock::new(|event: *mut AnyObject| -> *mut AnyObject {
            close_select_on_outside_click(event);
            event
        });
    // LeftMouseDown(1) | RightMouseDown(3) | OtherMouseDown(25); the mask is 1 << type.
    let mask: u64 = (1u64 << 1) | (1u64 << 3) | (1u64 << 25);
    let monitor: *mut AnyObject = msg_send![
        class!(NSEvent),
        addLocalMonitorForEventsMatchingMask: mask,
        handler: &*handler
    ];
    if !monitor.is_null() {
        *SELECT_MONITOR.lock().unwrap() = Some(SelectMonitor { monitor, handler });
    }
}

/// Remove the monitor (no longer needed once the dropdown is closed).
unsafe fn remove_select_monitor() {
    let taken = SELECT_MONITOR.lock().unwrap().take();
    if let Some(installed) = taken {
        let _: () = msg_send![class!(NSEvent), removeMonitor: installed.monitor];
    }
}

/// Whether a rect contains a point, in screen coordinates.
fn rect_contains_point(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y <= rect.origin.y + rect.size.height
}

/// A view's rect in screen coordinates (None when the view is not in a window).
unsafe fn view_rect_on_screen(view: *mut AnyObject, rect: NSRect) -> Option<NSRect> {
    if view.is_null() {
        return None;
    }
    let window: *mut AnyObject = msg_send![view, window];
    if window.is_null() {
        return None;
    }
    let in_window: NSRect = msg_send![
        view,
        convertRect: rect,
        toView: std::ptr::null::<AnyObject>()
    ];
    Some(msg_send![window, convertRectToScreen: in_window])
}

/// Whether a mouse-down landed outside the dropdown; if so, close the active select. Monitor
/// callback, runs on the main thread.
unsafe fn close_select_on_outside_click(event: *mut AnyObject) {
    let active = *ACTIVE_SETTINGS_SELECT.lock().unwrap();
    let Some(active) = active else { return };
    let button = active as *mut AnyObject;
    let panel = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&active)
        .map(|state| state.panel)
        .unwrap_or(0) as *mut AnyObject;
    if button.is_null() {
        return;
    }
    // Convert the click to screen coordinates; an event that belongs to no window counts as a click outside.
    let event_window: *mut AnyObject = msg_send![event, window];
    if event_window.is_null() {
        settings_select_close(button);
        return;
    }
    let in_window: NSPoint = msg_send![event, locationInWindow];
    let screen_point: NSPoint = msg_send![event_window, convertPointToScreen: in_window];

    // The panel itself -- its rect, not the window's: the window keeps 12pt of shadow padding
    // around it, and a click in that transparent ring counts as outside.
    if !panel.is_null() {
        let bounds: NSRect = msg_send![panel, bounds];
        if let Some(rect) = view_rect_on_screen(panel, bounds) {
            if rect_contains_point(rect, screen_point) {
                return;
            }
        }
    }
    // The trigger: the button's own mouseDown toggles it, so this must not interfere (otherwise it
    // would close and immediately reopen).
    let button_bounds: NSRect = msg_send![button, bounds];
    if let Some(rect) = view_rect_on_screen(button, button_bounds) {
        if rect_contains_point(rect, screen_point) {
            return;
        }
    }
    settings_select_close(button);
}

/// Close and release an option popup window.
unsafe fn close_select_popup(popup: *mut AnyObject) {
    if popup.is_null() {
        return;
    }
    let parent: *mut AnyObject = msg_send![popup, parentWindow];
    if !parent.is_null() {
        let _: () = msg_send![parent, removeChildWindow: popup];
    }
    let _: () = msg_send![popup, orderOut: std::ptr::null_mut::<AnyObject>()];
    let _: () = msg_send![popup, close];
    release_obj(popup);
}

/// Paint an option row according to its selected/hovered state.
unsafe fn settings_select_item_apply_background(item: *mut AnyObject, hovered: bool) {
    let select: *mut AnyObject = msg_send![item, target];
    let index: isize = msg_send![item, tag];
    let selected = !select.is_null()
        && SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(select as usize))
            .is_some_and(|state| state.selected == index);
    let layer: *mut AnyObject = msg_send![item, layer];
    if layer.is_null() {
        return;
    }
    let palette = settings_palette();
    let background = if selected || hovered {
        settings_select_item_active_color(palette)
    } else {
        0x00000000
    };
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
    let tint = crate::ffi::hex_to_ns_color(if selected || hovered {
        palette.primary_text
    } else {
        palette.muted_text
    });
    let _: () = msg_send![item, setContentTintColor: tint];
    let label = SETTINGS_SELECT_ITEM_LABEL_VIEWS
        .lock()
        .unwrap()
        .get(&(item as usize))
        .copied()
        .unwrap_or(0) as *mut AnyObject;
    if !label.is_null() {
        let _: () = msg_send![label, setTextColor: tint];
    }
}

extern "C" fn settings_select_item_mouse_entered(
    this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        settings_select_item_apply_background(this as *mut AnyObject, true);
    }
}

extern "C" fn settings_select_item_mouse_exited(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        settings_select_item_apply_background(this as *mut AnyObject, false);
    }
}

extern "C" fn settings_select_item_reveal(this: *mut c_void, _cmd: Sel, _object: *mut c_void) {
    unsafe {
        let item = this as *mut AnyObject;
        let _: () = msg_send![item, setAlphaValue: 1.0f64];
        let layer: *mut AnyObject = msg_send![item, layer];
        if !layer.is_null() {
            let key_path = make_nsstring("opacity");
            let animation: *mut AnyObject = msg_send![
                class!(CABasicAnimation),
                animationWithKeyPath: key_path
            ];
            CFRelease(key_path as *const c_void);
            let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
            let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
            let _: () = msg_send![animation, setFromValue: from];
            let _: () = msg_send![animation, setToValue: to];
            let _: () = msg_send![animation, setDuration: 0.16f64];
            let animation_key = make_nsstring("settings-select-item-reveal");
            let _: () = msg_send![layer, addAnimation: animation, forKey: animation_key];
            CFRelease(animation_key as *const c_void);
        }
    }
}

unsafe fn settings_select_remove_item_labels(panel: *mut AnyObject) {
    let mut labels = SETTINGS_SELECT_ITEM_LABEL_VIEWS.lock().unwrap();
    for item in settings_select_item_views(panel) {
        labels.remove(&(item as usize));
    }
}

/// The popup window is never the key window, so a click on it counts as a first mouse: option rows
/// must accept it, otherwise AppKit swallows the first click (the row appears to need two clicks).
extern "C" fn settings_select_item_accepts_first_mouse(
    _this: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) -> bool {
    true
}

fn settings_select_item_class() -> *mut AnyObject {
    SETTINGS_SELECT_ITEM_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsSelectItem").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                settings_select_item_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                settings_select_item_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(reveal),
                settings_select_item_reveal as *mut c_void,
                CString::new("v@:").unwrap().as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(acceptsFirstMouse:),
                settings_select_item_accepts_first_mouse as *mut c_void,
                CString::new("B@:@").unwrap().as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsSelectItemClass(cls)
        })
        .0
}

#[allow(clippy::too_many_arguments)]
unsafe fn settings_select_make_item(
    select: *mut AnyObject,
    panel: *mut AnyObject,
    index: usize,
    title: &str,
    selected: bool,
    width: f64,
    y: f64,
    row_h: f64,
) {
    let item_w = (width - 8.0).max(1.0);
    let item: *mut AnyObject = msg_send![settings_select_item_class(), alloc];
    let item: *mut AnyObject = msg_send![
        item,
        initWithFrame: NSRect::new(
            NSPoint::new(4.0, y),
            NSSize::new(item_w, row_h)
        )
    ];
    let _: () = msg_send![item, setButtonType: 0isize];
    let _: () = msg_send![item, setBordered: false];
    let title_ns = make_nsstring("");
    let _: () = msg_send![item, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![item, setAlignment: 0isize]; // NSTextAlignmentLeft
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![item, setFont: font];
    let _: () = msg_send![item, setTag: index as isize];
    let _: () = msg_send![item, setTarget: select];
    let _: () = msg_send![item, setAction: sel!(selectOption:)];
    let symbol = SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&(select as usize))
        .and_then(|state| state.item_symbols.get(index))
        .and_then(|symbol| symbol.as_deref())
        .map(str::to_owned);
    let has_symbol = symbol.is_some();
    if let Some(symbol) = symbol {
        let image = make_symbol_image(&symbol, NSSize::new(16.0, 16.0));
        if !image.is_null() {
            let _: () = msg_send![item, setImage: image];
            let _: () = msg_send![item, setImagePosition: 2isize]; // NSImageLeft
        }
    }
    // Use the same wrapped text treatment as the selected value above instead of the native
    // NSButton title, whose cell remains single-line and clips long options.
    let label_x = if has_symbol { 28.0 } else { 8.0 };
    let label_w = (item_w - label_x - 30.0).max(1.0);
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![
        label,
        initWithFrame: NSRect::new(
            NSPoint::new(label_x, 0.0),
            NSSize::new(label_w, row_h.max(1.0))
        )
    ];
    let title_ns = make_nsstring(title);
    let _: () = msg_send![label, setStringValue: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setAlignment: 0isize]; // NSTextAlignmentLeft
    let _: () = msg_send![label, setUsesSingleLineMode: false];
    let _: () = msg_send![label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    if msg_send![label, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![label, setMaximumNumberOfLines: 0isize];
    }
    let _: () = msg_send![label, setPreferredMaxLayoutWidth: label_w];
    let cell: *mut AnyObject = msg_send![label, cell];
    if !cell.is_null() && msg_send![cell, respondsToSelector: sel!(setTruncatesLastVisibleLine:)] {
        let _: () = msg_send![cell, setTruncatesLastVisibleLine: false];
    }
    let wrapped_size: NSSize = msg_send![label, sizeThatFits: NSSize::new(label_w, 10_000.0)];
    let text_h = if wrapped_size.height.is_finite() && wrapped_size.height > 0.0 {
        wrapped_size.height.min(row_h).max(1.0)
    } else {
        row_h.max(1.0)
    };
    let _: () = msg_send![
        label,
        setFrame: NSRect::new(
            NSPoint::new(label_x, (row_h - text_h).max(0.0) / 2.0),
            NSSize::new(label_w, text_h)
        )
    ];
    let _: () = msg_send![item, addSubview: label];
    SETTINGS_SELECT_ITEM_LABEL_VIEWS
        .lock()
        .unwrap()
        .insert(item as usize, label as usize);
    release_obj(label);
    let _: () = msg_send![item, setWantsLayer: true];
    let item_layer: *mut AnyObject = msg_send![item, layer];
    if !item_layer.is_null() {
        let _: () = msg_send![item_layer, setCornerRadius: 8.0f64];
        let _: () = msg_send![item_layer, setMasksToBounds: true];
    }
    settings_select_item_apply_background(item, false);
    if selected {
        let check_frame = NSRect::new(
            NSPoint::new((item_w - 24.0).max(0.0), (row_h - 14.0).max(0.0) / 2.0),
            NSSize::new(14.0, 14.0),
        );
        let check = make_symbol_image_view("checkmark", check_frame);
        let _: () = msg_send![item, addSubview: check];
        release_obj(check);
    }
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(item_w, row_h)),
        options: 0x01u64 | 0x80u64 | 0x200u64,
        owner: item,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![item, addTrackingArea: tracking];
    release_obj(tracking);
    let _: () = msg_send![panel, addSubview: item];
    let _: () = msg_send![item, setAlphaValue: 0.0f64];
    let reveal_delay = 0.05 + index as f64 * 0.035;
    let _: () = msg_send![
        item,
        performSelector: sel!(reveal),
        withObject: std::ptr::null::<AnyObject>(),
        afterDelay: reveal_delay
    ];
    release_obj(item);
}

/// The resolved option-panel geometry.
#[derive(Debug, PartialEq)]
struct SettingsSelectPanel {
    /// The height a fully expanded option list needs.
    natural_h: f64,
    /// The panel's final height (already capped to the host's available height).
    panel_h: f64,
    panel_y: f64,
    /// Opens upwards (the entrance animation slides in from that side).
    opens_above: bool,
    /// Does not fit -> the rows need to go into a scroll view.
    scrolls: bool,
}

/// Blank space the floating window leaves around the panel, so the panel's rounded shadow has room
/// (a window clips whatever exceeds its own frame).
const SELECT_POPUP_SHADOW_PAD: f64 = 12.0;

/// Resolve the geometry of an opening option panel. Pure, so it can be unit-tested (see this
/// module's tests).
///
/// The coordinate space is the SCREEN: a dropdown is a floating layer, so its size and direction
/// follow the screen's visible area. It used to be bounded by the host window (the recording panel
/// is only 440x240), which clipped the top edge when there were many options -- that is how the
/// first of the eight options disappeared.
fn settings_select_panel_geometry(
    trigger: NSRect,
    bounds: NSRect,
    item_count: usize,
    row_h: f64,
) -> SettingsSelectPanel {
    const MARGIN: f64 = 4.0;
    const GAP: f64 = 8.0;
    let natural_h = item_count as f64 * row_h + 8.0;
    let bounds_top = bounds.origin.y + bounds.size.height;
    // How much room the trigger really has above/below (minus the gap and the inset).
    let below = trigger.origin.y - bounds.origin.y;
    let above = bounds_top - (trigger.origin.y + trigger.size.height);
    let room_below = (below - GAP - MARGIN).max(0.0);
    let room_above = (above - GAP - MARGIN).max(0.0);
    let opens_above = room_below < natural_h && room_above > room_below;
    let room = if opens_above { room_above } else { room_below };
    // Keep at least one row so a pathologically short host cannot produce degenerate geometry.
    let min_h = row_h + 8.0;
    let panel_h = natural_h.min(room).max(min_h);
    let panel_y = if opens_above {
        trigger.origin.y + trigger.size.height + GAP
    } else {
        trigger.origin.y - GAP - panel_h
    };
    let panel_y = panel_y.clamp(
        bounds.origin.y + MARGIN,
        (bounds_top - panel_h - MARGIN).max(bounds.origin.y + MARGIN),
    );
    SettingsSelectPanel {
        natural_h,
        panel_h,
        panel_y,
        opens_above,
        scrolls: panel_h + 0.5 < natural_h,
    }
}

unsafe fn settings_select_open(button: *mut AnyObject) {
    let window: *mut AnyObject = msg_send![button, window];
    if window.is_null() {
        return;
    }

    if let Some(active) = *ACTIVE_SETTINGS_SELECT.lock().unwrap() {
        if active != button as usize {
            settings_select_close(active as *mut AnyObject);
        }
    }

    // A fast reopen can happen while the close fade is still pending. Remove only the stale
    // panel that belongs to this control and cancel its delayed cleanup callback.
    let (stale_panel, stale_popup) = {
        let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
        states
            .get_mut(&(button as usize))
            .filter(|state| !state.open)
            .map(|state| {
                let panel = state.panel;
                let popup = state.popup;
                state.panel = 0;
                state.popup = 0;
                (panel as *mut AnyObject, popup as *mut AnyObject)
            })
            .unwrap_or((std::ptr::null_mut(), std::ptr::null_mut()))
    };
    if !stale_panel.is_null() {
        let _: () = msg_send![
            class!(NSObject),
            cancelPreviousPerformRequestsWithTarget: button,
            selector: sel!(finishClose:),
            object: stale_panel
        ];
        settings_select_remove_item_labels(stale_panel);
        let _: () = msg_send![stale_panel, removeFromSuperview];
    }
    // The stale popup window must go too: its finishClose was just cancelled, so nothing else will
    // release that +1.
    close_select_popup(stale_popup);

    let (items, selected) = {
        let states = SETTINGS_SELECT_STATES.lock().unwrap();
        let Some(state) = states.get(&(button as usize)) else {
            return;
        };
        if state.items.is_empty() || state.open {
            return;
        }
        (state.items.clone(), state.selected.max(0) as usize)
    };

    // Both the trigger and the available area are resolved in SCREEN coordinates: a dropdown is a
    // floating layer, so only the screen's visible area bounds its size and direction -- not the
    // host window (the recording panel is only 440x240).
    let bounds: NSRect = msg_send![button, bounds];
    let trigger_in_window: NSRect = msg_send![
        button,
        convertRect: bounds,
        toView: std::ptr::null_mut::<AnyObject>()
    ];
    let trigger: NSRect = msg_send![window, convertRectToScreen: trigger_in_window];
    let screen: *mut AnyObject = msg_send![window, screen];
    let screen: *mut AnyObject = if screen.is_null() {
        msg_send![class!(NSScreen), mainScreen]
    } else {
        screen
    };
    let screen_visible: NSRect = if screen.is_null() {
        trigger
    } else {
        msg_send![screen, visibleFrame]
    };
    let panel_w = trigger.size.width.max(120.0);
    let row_h = settings_select_required_option_row_height(panel_w, &items);
    let geometry = settings_select_panel_geometry(trigger, screen_visible, items.len(), row_h);
    let natural_h = geometry.natural_h;
    let panel_h = geometry.panel_h;
    let panel_y = geometry.panel_y;
    // The panel lives inside its own floating window with the shadow padding around it; the window
    // itself is placed at (trigger.x, panel_y) on screen.
    let panel: *mut AnyObject = msg_send![class!(NSView), alloc];
    let panel: *mut AnyObject = msg_send![
        panel,
        initWithFrame: NSRect::new(
            NSPoint::new(SELECT_POPUP_SHADOW_PAD, SELECT_POPUP_SHADOW_PAD),
            NSSize::new(panel_w, panel_h)
        )
    ];
    let _: () = msg_send![panel, setWantsLayer: true];
    let panel_layer: *mut AnyObject = msg_send![panel, layer];
    if !panel_layer.is_null() {
        let palette = settings_palette();
        crate::ffi::layer_set_background(
            panel_layer,
            crate::ffi::hex_to_cg_color(settings_select_surface_color(palette)),
        );
        crate::ffi::layer_set_border(
            panel_layer,
            crate::ffi::hex_to_cg_color(palette.card_border),
        );
        let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![panel_layer, setCornerRadius: 12.0f64];
        // Keep the panel's shadow outside its bounds; option rows already clip themselves to
        let _: () = msg_send![panel_layer, setMasksToBounds: false];
        let shadow_color = crate::ffi::hex_to_cg_color(0x000000FF);
        crate::ffi::layer_set_shadow_color(panel_layer, shadow_color);
        let _: () = msg_send![panel_layer, setShadowOpacity: 0.12f32];
        let _: () = msg_send![panel_layer, setShadowRadius: 8.0f64];
        let _: () = msg_send![panel_layer, setShadowOffset: NSSize::new(0.0, -4.0)];
    }

    // Where the rows hang: directly on the panel when they fit, otherwise inside a scroll view
    // (see the note on panel_h above).
    let list_parent = if geometry.scrolls {
        let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
        let scroll: *mut AnyObject = msg_send![
            scroll,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(panel_w, panel_h))
        ];
        let _: () = msg_send![scroll, setBorderType: 0u64];
        let _: () = msg_send![scroll, setDrawsBackground: false];
        let _: () = msg_send![scroll, setHasHorizontalScroller: false];
        let _: () = msg_send![scroll, setHasVerticalScroller: true];
        let _: () = msg_send![scroll, setAutohidesScrollers: true];
        let _: () = msg_send![scroll, setScrollerStyle: 1isize]; // overlay
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let _: () = msg_send![clip, setDrawsBackground: false];
        let document: *mut AnyObject = msg_send![class!(NSView), alloc];
        let document: *mut AnyObject = msg_send![
            document,
            initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(panel_w, natural_h))
        ];
        let _: () = msg_send![scroll, setDocumentView: document];
        let _: () = msg_send![panel, addSubview: scroll];
        // The document is not flipped, so a scroll view starts at its bottom; scroll to the top
        // explicitly so the first option is visible (same as make_settings_page does for the page).
        let top_origin = (natural_h - panel_h).max(0.0);
        let _: () = msg_send![clip, scrollToPoint: NSPoint::new(0.0, top_origin)];
        let _: () = msg_send![scroll, reflectScrolledClipView: clip];
        release_obj(document);
        release_obj(scroll);
        document
    } else {
        panel
    };

    for (index, title) in items.iter().enumerate() {
        // The row's position in the full list (natural_h == panel_h when nothing is scrolled, which
        // matches the previous formula).
        let item_y = natural_h - 4.0 - (index as f64 + 1.0) * row_h;
        settings_select_make_item(
            button,
            list_parent,
            index,
            title,
            index == selected,
            panel_w,
            item_y,
            row_h,
        );
    }
    // Container: the window is one shadow pad larger than the panel so its rounded shadow has room
    // (a window clips whatever exceeds its own frame).
    let padded_w = panel_w + 2.0 * SELECT_POPUP_SHADOW_PAD;
    let padded_h = panel_h + 2.0 * SELECT_POPUP_SHADOW_PAD;
    let container: *mut AnyObject = msg_send![class!(NSView), alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(padded_w, padded_h))
    ];
    let _: () = msg_send![container, addSubview: panel];
    release_obj(panel);

    // Borderless + non-activating panel: the popup never takes focus (keyboard navigation stays on
    // the host's button) and never activates the app.
    let popup: *mut AnyObject = msg_send![class!(NSPanel), alloc];
    let popup: *mut AnyObject = msg_send![
        popup,
        initWithContentRect: NSRect::new(
            NSPoint::new(
                trigger.origin.x - SELECT_POPUP_SHADOW_PAD,
                panel_y - SELECT_POPUP_SHADOW_PAD
            ),
            NSSize::new(padded_w, padded_h)
        ),
        styleMask: 128u64, // NSWindowStyleMaskNonactivatingPanel
        backing: 2u64,
        defer: false
    ];
    let _: () = msg_send![popup, setOpaque: false];
    let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![popup, setBackgroundColor: clear_color];
    let _: () = msg_send![popup, setHasShadow: false];
    let _: () = msg_send![popup, setReleasedWhenClosed: false];
    // Sits above the host, which may itself be a floating window (the recording panel is level 3).
    let host_level: isize = msg_send![window, level];
    let _: () = msg_send![popup, setLevel: host_level + 1];
    let _: () = msg_send![popup, setContentView: container];
    release_obj(container);
    // Attached as a child of the host, so it disappears with the host instead of lingering.
    let _: () = msg_send![window, addChildWindow: popup, ordered: 1isize]; // NSWindowAbove
    let _: () = msg_send![popup, orderFront: std::ptr::null_mut::<AnyObject>()];
    {
        let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
        if let Some(state) = states.get_mut(&(button as usize)) {
            state.panel = panel as usize;
            state.popup = popup as usize;
            state.open = true;
        }
    }
    *ACTIVE_SETTINGS_SELECT.lock().unwrap() = Some(button as usize);
    settings_select_apply_visual(button, true);
    // Catch click-outside while open: the dropdown lives in its own window, so the host window's
    // click path no longer covers it.
    install_select_monitor();

    if !window.is_null() {
        let _: bool = msg_send![window, makeFirstResponder: button];
    }

    // The reference unfolds with opacity rather than scaling the whole panel, keeping text and
    if !panel_layer.is_null() {
        let _: () = msg_send![panel_layer, setOpacity: 0.0f32];
        let key_path = make_nsstring("opacity");
        let animation: *mut AnyObject = msg_send![
            class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        CFRelease(key_path as *const c_void);
        let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
        let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 1.0f64];
        let _: () = msg_send![animation, setFromValue: from];
        let _: () = msg_send![animation, setToValue: to];
        let _: () = msg_send![animation, setDuration: 0.18f64];
        let _: () = msg_send![panel_layer, setOpacity: 1.0f32];
        let animation_key = make_nsstring("settings-select-open");
        let _: () = msg_send![panel_layer, addAnimation: animation, forKey: animation_key];
        CFRelease(animation_key as *const c_void);

        // Separate the panel from the trigger with a short spring translation, matching the
        // reference's attached-then-detached unfold without scaling the panel contents.
        let key_path = make_nsstring("transform.translation.y");
        let spring: *mut AnyObject = msg_send![
            class!(CASpringAnimation),
            animationWithKeyPath: key_path
        ];
        CFRelease(key_path as *const c_void);
        let from_y = if geometry.opens_above { -8.0 } else { 8.0 };
        let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: from_y];
        let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
        let _: () = msg_send![spring, setFromValue: from];
        let _: () = msg_send![spring, setToValue: to];
        let _: () = msg_send![spring, setMass: 1.0f64];
        let _: () = msg_send![spring, setStiffness: 260.0f64];
        let _: () = msg_send![spring, setDamping: 24.0f64];
        let _: () = msg_send![spring, setInitialVelocity: 0.0f64];
        let duration: f64 = msg_send![spring, settlingDuration];
        let _: () = msg_send![spring, setDuration: duration.max(0.32)];
        let animation_key = make_nsstring("settings-select-open-translation");
        let _: () = msg_send![panel_layer, addAnimation: spring, forKey: animation_key];
        CFRelease(animation_key as *const c_void);
    }
}

pub(super) extern "C" fn settings_select_select_option(
    this: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    unsafe {
        let select = this as *mut AnyObject;
        let item = sender as *mut AnyObject;
        let index: isize = msg_send![item, tag];
        let valid = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(select as usize))
            .is_some_and(|state| index >= 0 && (index as usize) < state.items.len());
        if !valid {
            return;
        }
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(select as usize))
        {
            state.selected = index;
        }
        settings_select_close(select);
        // Defer the target/action callback until the option's mouse event has fully unwound.
        // Locale changes rebuild the settings window; doing that inside the old popup's event
        // stack lets AppKit order the newly rebuilt window out again when menu tracking ends.
        let _: () = msg_send![
            select,
            performSelector: sel!(sendPendingAction),
            withObject: std::ptr::null::<AnyObject>(),
            afterDelay: 0.0f64
        ];
    }
}

/// Dispatch a select action after the popup event has returned to the run loop.
extern "C" fn settings_select_send_pending_action(this: *mut c_void, _cmd: Sel) {
    unsafe {
        let select = this as *mut AnyObject;
        let target: *mut AnyObject = msg_send![select, target];
        if !target.is_null() {
            let action: Sel = msg_send![select, action];
            let _: bool = msg_send![select, sendAction: action, to: target];
        }
    }
}

extern "C" fn settings_select_mouse_down(this: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        if !msg_send![button, isEnabled] {
            return;
        }
        let (open, popup) = {
            let states = SETTINGS_SELECT_STATES.lock().unwrap();
            match states.get(&(button as usize)) {
                Some(state) => (state.open, state.popup as *mut AnyObject),
                None => (false, std::ptr::null_mut()),
            }
        };
        // The popup is a child window of the host: it hides together with the host, and the `open`
        // flag then goes stale. Treat it as closed and tidy up so this click opens a fresh popup --
        // otherwise the click is spent closing something invisible (the control would need two
        // clicks to open).
        let stale = open && (popup.is_null() || !msg_send![popup, isVisible]);
        if stale {
            if let Some(state) = SETTINGS_SELECT_STATES
                .lock()
                .unwrap()
                .get_mut(&(button as usize))
            {
                state.open = false;
                state.panel = 0;
                state.popup = 0;
            }
            if *ACTIVE_SETTINGS_SELECT.lock().unwrap() == Some(button as usize) {
                *ACTIVE_SETTINGS_SELECT.lock().unwrap() = None;
            }
            close_select_popup(popup);
            settings_select_apply_visual(button, false);
        }
        if open && !stale {
            settings_select_close(button);
        } else {
            settings_select_open(button);
        }
    }
}

extern "C" fn settings_select_key_down(this: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let key_code: u16 = msg_send![event as *mut AnyObject, keyCode];
        if matches!(key_code, 36 | 49 | 125 | 126) {
            let open = SETTINGS_SELECT_STATES
                .lock()
                .unwrap()
                .get(&(button as usize))
                .is_some_and(|state| state.open);
            if key_code == 36 || key_code == 49 {
                if open {
                    settings_select_close(button);
                } else {
                    settings_select_open(button);
                }
            } else if !open {
                settings_select_open(button);
            }
            return;
        }
        if key_code == 53 {
            settings_select_close(button);
            return;
        }
        let events: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: event];
        let _: () = msg_send![button, interpretKeyEvents: events];
    }
}

extern "C" fn settings_select_accepts_first_responder(_this: *mut c_void, _cmd: Sel) -> bool {
    true
}

extern "C" fn settings_select_index(this: *mut c_void, _cmd: Sel) -> isize {
    SETTINGS_SELECT_STATES
        .lock()
        .unwrap()
        .get(&(this as usize))
        .map(|state| state.selected)
        .unwrap_or(-1)
}

extern "C" fn settings_select_set_index(this: *mut c_void, _cmd: Sel, index: isize) {
    unsafe {
        let button = this as *mut AnyObject;
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
        {
            if index >= 0 && (index as usize) < state.items.len() {
                state.selected = index;
            }
        }
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        settings_select_apply_visual(button, open);
    }
}

extern "C" fn settings_select_remove_all(this: *mut c_void, _cmd: Sel) {
    unsafe {
        let button = this as *mut AnyObject;
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        if open {
            settings_select_close(button);
        }
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
        {
            state.items.clear();
            state.item_symbols.clear();
            state.selected = -1;
        }
        settings_select_apply_visual(button, false);
    }
}

extern "C" fn settings_select_add_item(this: *mut c_void, _cmd: Sel, title: *mut c_void) {
    unsafe {
        let button = this as *mut AnyObject;
        let title = nsstring_to_rust(title as *mut AnyObject);
        if let Some(state) = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get_mut(&(button as usize))
        {
            state.items.push(title);
            state.item_symbols.push(None);
            if state.selected < 0 {
                state.selected = 0;
            }
        }
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(button as usize))
            .is_some_and(|state| state.open);
        settings_select_apply_visual(button, open);
    }
}

/// Attach an optional SF Symbol to one item in the next rendered options panel.
pub(super) fn settings_select_set_item_symbol(select: *mut AnyObject, index: usize, symbol: &str) {
    let mut states = SETTINGS_SELECT_STATES.lock().unwrap();
    let Some(state) = states.get_mut(&(select as usize)) else {
        return;
    };
    if let Some(slot) = state.item_symbols.get_mut(index) {
        *slot = Some(symbol.to_owned());
    }
}

extern "C" fn settings_select_set_enabled(this: *mut c_void, _cmd: Sel, enabled: bool) {
    unsafe {
        let mut sup = ObjcSuper {
            receiver: this,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        type SetEnabled = unsafe extern "C" fn(*mut ObjcSuper, Sel, bool);
        let send: SetEnabled = std::mem::transmute(objc_msgSendSuper as *const ());
        send(&mut sup, sel!(setEnabled:), enabled);
        let open = SETTINGS_SELECT_STATES
            .lock()
            .unwrap()
            .get(&(this as usize))
            .is_some_and(|state| state.open);
        settings_select_apply_visual(this as *mut AnyObject, open);
    }
}

fn settings_select_class() -> *mut AnyObject {
    SETTINGS_SELECT_CLASS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsSelect").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types_void_event = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseDown:),
                settings_select_mouse_down as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(keyDown:),
                settings_select_key_down as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(selectOption:),
                settings_select_select_option as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(sendPendingAction),
                settings_select_send_pending_action as *mut c_void,
                CString::new("v@:").unwrap().as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(finishClose:),
                settings_select_finish_close as *mut c_void,
                types_void_event.as_ptr(),
            );
            let types_bool = CString::new("B@:").unwrap();
            class_addMethod(
                cls,
                sel!(acceptsFirstResponder),
                settings_select_accepts_first_responder as *mut c_void,
                types_bool.as_ptr(),
            );
            let types_index = CString::new("q@:").unwrap();
            class_addMethod(
                cls,
                sel!(indexOfSelectedItem),
                settings_select_index as *mut c_void,
                types_index.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(selectItemAtIndex:),
                settings_select_set_index as *mut c_void,
                CString::new("v@:q").unwrap().as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(removeAllItems),
                settings_select_remove_all as *mut c_void,
                CString::new("v@:").unwrap().as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(addItemWithTitle:),
                settings_select_add_item as *mut c_void,
                types_void_event.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(setEnabled:),
                settings_select_set_enabled as *mut c_void,
                CString::new("v@:B").unwrap().as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsSelectClass(cls)
        })
        .0
}

/// Custom settings select with a bouncy, position-aware options panel (alloc +1).
pub(super) unsafe fn make_popup(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    items: &[&str],
    selected: usize,
) -> *mut AnyObject {
    let popup: *mut AnyObject = msg_send![settings_select_class(), alloc];
    let popup: *mut AnyObject = msg_send![
        popup,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
    ];
    let _: () = msg_send![popup, setButtonType: 0isize];
    let _: () = msg_send![popup, setBordered: false];
    let _: () = msg_send![popup, setAlignment: 0isize]; // NSTextAlignmentLeft
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![popup, setFont: font];
    let _: () = msg_send![popup, setFocusRingType: 1isize]; // NSFocusRingTypeNone
    let _: () = msg_send![popup, setWantsLayer: true];
    SETTINGS_SELECT_STATES.lock().unwrap().insert(
        popup as usize,
        SettingsSelectState {
            items: items.iter().map(|item| (*item).to_owned()).collect(),
            item_symbols: vec![None; items.len()],
            selected: selected as isize,
            panel: 0,
            popup: 0,
            open: false,
        },
    );
    let _: () = msg_send![popup, setEnabled: true];
    settings_select_apply_visual(popup, false);
    if let Some(first) = items.first() {
        settings_select_set_title(popup, first);
        let _: () = msg_send![popup, selectItemAtIndex: selected as isize];
    }
    popup
}

/// Measure the largest trigger height required by a select's candidate values.
pub(super) unsafe fn settings_select_required_control_height(
    width: f64,
    items: &[&str],
    minimum_height: f64,
) -> f64 {
    let label_width = (width - 12.0 - 16.0 - 8.0 - 12.0).max(1.0);
    let field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let field: *mut AnyObject = msg_send![
        field,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(label_width, minimum_height.max(1.0)),
        )
    ];
    let _: () = msg_send![field, setBezeled: false];
    let _: () = msg_send![field, setDrawsBackground: false];
    let _: () = msg_send![field, setEditable: false];
    let _: () = msg_send![field, setSelectable: false];
    let _: () = msg_send![field, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    let _: () = msg_send![field, setUsesSingleLineMode: false];
    if msg_send![field, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![field, setMaximumNumberOfLines: 0isize];
    }
    let _: () = msg_send![field, setPreferredMaxLayoutWidth: label_width];
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 13.5f64];
    let _: () = msg_send![field, setFont: font];

    let mut required_height = minimum_height.max(1.0);
    for item in items {
        let item_ns = make_nsstring(item);
        let _: () = msg_send![field, setStringValue: item_ns];
        CFRelease(item_ns as *const c_void);
        let measured: NSSize = msg_send![
            field,
            sizeThatFits: NSSize::new(label_width, 10_000.0)
        ];
        if measured.height.is_finite() && measured.height > 0.0 {
            required_height = required_height.max(measured.height.ceil());
        }
    }
    release_obj(field);
    required_height
}

/// Measure one shared option-row height for every value in a select's menu.
unsafe fn settings_select_required_option_row_height(width: f64, items: &[String]) -> f64 {
    let items: Vec<&str> = items.iter().map(String::as_str).collect();
    let text_height = settings_select_required_control_height(width, &items, 1.0);
    (text_height + 8.0).ceil().max(32.0)
}

#[cfg(test)]
mod tests {
    use super::{
        settings_select_centered_text_geometry, settings_select_needs_wrap,
        settings_select_panel_geometry,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    fn rect_inside(outer: NSRect, inner: NSRect, epsilon: f64) -> bool {
        inner.origin.x >= outer.origin.x - epsilon
            && inner.origin.y >= outer.origin.y - epsilon
            && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + epsilon
            && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + epsilon
    }

    #[test]
    fn settings_select_centers_single_line_and_wraps_only_when_needed() {
        assert!(!settings_select_needs_wrap(98.0, 152.0, false));
        assert!(settings_select_needs_wrap(175.0, 152.0, false));
        assert!(settings_select_needs_wrap(40.0, 152.0, true));
        assert_eq!(
            settings_select_centered_text_geometry(34.0, 16.0),
            (9.0, 16.0)
        );
        assert_eq!(
            settings_select_centered_text_geometry(34.0, 32.0),
            (1.0, 32.0)
        );
    }

    #[test]
    fn settings_select_panel_is_bounded_by_the_screen_not_the_host_window() {
        // The real case: the action dropdown sits in the recording panel (440x240), but the panel is
        // now its own floating window, so the eight options (264pt) open within the screen's visible
        // area: fully shown, no scrolling, no clipping -- however small the host is.
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1512.0, 944.0));
        // The trigger's screen position while the recording panel sits in the lower half.
        let trigger = NSRect::new(NSPoint::new(600.0, 300.0), NSSize::new(290.0, 26.0));
        let panel = settings_select_panel_geometry(trigger, screen, 8, 32.0);

        assert!(!panel.scrolls, "264pt fits on a 944pt screen");
        assert_eq!(panel.panel_h, panel.natural_h);
        let frame = NSRect::new(
            NSPoint::new(trigger.origin.x, panel.panel_y),
            NSSize::new(290.0, panel.panel_h),
        );
        assert!(
            rect_inside(screen, frame, 0.001),
            "the panel must stay inside the screen: {panel:?}"
        );
    }

    #[test]
    fn settings_select_panel_still_scrolls_when_the_screen_cannot_fit_it() {
        // When even the screen cannot fit it (an extreme case), it still caps to the available
        // height and scrolls internally instead of overflowing and getting clipped.
        let screen = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0));
        let trigger = NSRect::new(NSPoint::new(40.0, 140.0), NSSize::new(290.0, 26.0));
        let panel = settings_select_panel_geometry(trigger, screen, 8, 32.0);

        assert!(panel.scrolls);
        assert!(panel.panel_h < panel.natural_h);
        assert!(panel.panel_h >= 40.0, "at least one row stays visible");
        let frame = NSRect::new(
            NSPoint::new(trigger.origin.x, panel.panel_y),
            NSSize::new(290.0, panel.panel_h),
        );
        assert!(
            rect_inside(screen, frame, 0.001),
            "the whole panel must stay inside the screen: {panel:?}"
        );
    }

    #[test]
    fn settings_select_panel_keeps_full_height_when_it_fits() {
        // In a roomy host (the main settings page) nothing changes: no scrolling, same position.
        let host = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(660.0, 760.0));
        let trigger = NSRect::new(NSPoint::new(420.0, 300.0), NSSize::new(200.0, 30.0));
        let panel = settings_select_panel_geometry(trigger, host, 8, 32.0);

        assert!(!panel.scrolls);
        assert_eq!(panel.panel_h, 8.0 * 32.0 + 8.0);
        assert_eq!(panel.panel_y, 300.0 - 8.0 - panel.panel_h);
    }
}
