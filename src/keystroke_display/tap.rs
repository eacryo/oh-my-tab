//! Read-only keyboard event tap. Event data is queued and never logged.

use std::ffi::c_void;

use crate::event_tap::{self, keyboard, tap_location, tap_options, tap_placement};

unsafe extern "C" fn callback(
    _proxy: event_tap::CGEventTapProxy,
    event_type: event_tap::CGEventType,
    event: event_tap::CGEventRef,
    _user_info: *mut c_void,
) -> event_tap::CGEventRef {
    if crate::input_monitor::handle_disabled_event(
        event_type,
        "keystroke-display",
        super::tap_control().is_stopping(),
    ) || !crate::input_monitor::taps_allowed()
        || !super::is_active()
    {
        return event;
    }

    let flags = event_tap::CGEventGetFlags(event);
    let input = match event_type {
        keyboard::EVENT_KEY_DOWN => {
            let keycode =
                event_tap::CGEventGetIntegerValueField(event, keyboard::FIELD_KEYCODE) as u16;
            let autorepeat =
                event_tap::CGEventGetIntegerValueField(event, keyboard::FIELD_AUTOREPEAT) != 0;
            let mut unicode = [0u16; 8];
            let mut length = 0u32;
            event_tap::CGEventKeyboardGetUnicodeString(
                event,
                unicode.len() as u32,
                &mut length,
                unicode.as_mut_ptr(),
            );
            let text =
                String::from_utf16_lossy(&unicode[..length.min(unicode.len() as u32) as usize]);
            super::secure::note_key_activity();
            super::Input::KeyDown {
                keycode,
                flags,
                autorepeat,
                unicode: text,
                glyph: super::state::KeyGlyph::EventUnicode,
            }
        }
        keyboard::EVENT_FLAGS_CHANGED => {
            let keycode =
                event_tap::CGEventGetIntegerValueField(event, keyboard::FIELD_KEYCODE) as u16;
            super::secure::note_key_activity();
            super::Input::FlagsChanged { flags, keycode }
        }
        _ => return event,
    };
    super::enqueue(input);
    event
}

pub(super) fn start(location: i32) -> std::thread::JoinHandle<()> {
    let mask = (1u64 << keyboard::EVENT_KEY_DOWN) | (1u64 << keyboard::EVENT_FLAGS_CHANGED);
    event_tap::start_event_tap_thread(
        location,
        tap_placement::HEAD_INSERT,
        tap_options::LISTEN_ONLY,
        mask,
        Some(callback),
        0,
        "keystroke-display",
        super::tap_control(),
        || {
            crate::log_debug!(
                "[keystroke-display] listen-only tap active at HEAD_INSERT after the switcher tap."
            );
        },
    )
}

pub(super) fn location_from_level(level: &str) -> i32 {
    match level {
        "hid" => tap_location::HID_EVENT_TAP,
        _ => tap_location::SESSION_EVENT_TAP,
    }
}
