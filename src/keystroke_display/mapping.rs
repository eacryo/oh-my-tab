//! Key labels plus the small main-thread keyboard-layout cache.

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use crate::event_tap::keyboard;
use crate::i18n::t;

const UC_KEY_ACTION_DOWN: u16 = 0;
const UC_KEY_TRANSLATE_NO_DEAD_KEYS: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum NamedKey {
    Static(&'static str),
    Localized(&'static str),
}

const NAMED_KEYS: &[(u16, NamedKey)] = &[
    (36, NamedKey::Static("↵")),
    (48, NamedKey::Static("⇥")),
    (
        keyboard::VK_SPACE,
        NamedKey::Localized("keystroke_display.key_space"),
    ),
    (51, NamedKey::Static("⌫")),
    (53, NamedKey::Static("esc")),
    (71, NamedKey::Localized("keystroke_display.key_clear")),
    (115, NamedKey::Localized("keystroke_display.key_home")),
    (116, NamedKey::Localized("keystroke_display.key_page_up")),
    (117, NamedKey::Static("⌦")),
    (119, NamedKey::Localized("keystroke_display.key_end")),
    (121, NamedKey::Localized("keystroke_display.key_page_down")),
    (123, NamedKey::Static("←")),
    (124, NamedKey::Static("→")),
    (125, NamedKey::Static("↓")),
    (126, NamedKey::Static("↑")),
    (122, NamedKey::Static("F1")),
    (120, NamedKey::Static("F2")),
    (99, NamedKey::Static("F3")),
    (118, NamedKey::Static("F4")),
    (96, NamedKey::Static("F5")),
    (97, NamedKey::Static("F6")),
    (98, NamedKey::Static("F7")),
    (100, NamedKey::Static("F8")),
    (101, NamedKey::Static("F9")),
    (109, NamedKey::Static("F10")),
    (103, NamedKey::Static("F11")),
    (111, NamedKey::Static("F12")),
    (105, NamedKey::Static("F13")),
    (107, NamedKey::Static("F14")),
    (113, NamedKey::Static("F15")),
    (106, NamedKey::Static("F16")),
    (64, NamedKey::Static("F17")),
    (79, NamedKey::Static("F18")),
    (80, NamedKey::Static("F19")),
    (90, NamedKey::Static("F20")),
];

pub(crate) fn named_key(keycode: u16) -> Option<String> {
    NAMED_KEYS
        .iter()
        .find(|(code, _)| *code == keycode)
        .map(|(_, name)| match name {
            NamedKey::Static(text) => (*text).to_string(),
            NamedKey::Localized(key) => t(key),
        })
}

pub(crate) fn modifier_glyphs(flags: u64) -> String {
    let mut output = String::with_capacity(8);
    if flags & keyboard::FLAG_COMMAND != 0 {
        output.push('⌘');
    }
    if flags & keyboard::FLAG_OPTION != 0 {
        output.push('⌥');
    }
    if flags & keyboard::FLAG_SHIFT != 0 {
        output.push('⇧');
    }
    if flags & keyboard::FLAG_CONTROL != 0 {
        output.push('⌃');
    }
    if flags & keyboard::FLAG_CAPS_LOCK != 0 {
        output.push('⇪');
    }
    if flags & keyboard::FLAG_FN != 0 {
        output.push_str("fn");
    }
    output
}

pub(crate) fn modifier_mask() -> u64 {
    keyboard::FLAG_COMMAND
        | keyboard::FLAG_OPTION
        | keyboard::FLAG_SHIFT
        | keyboard::FLAG_CONTROL
        | keyboard::FLAG_CAPS_LOCK
        | keyboard::FLAG_FN
}

/// Resolve a key label. Named keys and an unmodified layout translation win; nonzero modifiers
/// forbid falling back to event Unicode because the system may already have transformed it.
pub(crate) fn key_glyph(keycode: u16, event_unicode: &str, modifiers: u64) -> Option<String> {
    if let Some(name) = named_key(keycode) {
        return Some(name);
    }
    resolve_layout_glyph(event_unicode, translate_unmodified(keycode), modifiers)
}

fn resolve_layout_glyph(
    event_unicode: &str,
    translation: Result<Option<String>, ()>,
    modifiers: u64,
) -> Option<String> {
    match translation {
        Ok(Some(text)) => Some(text),
        Ok(None) if modifiers == 0 => {
            is_printable(event_unicode).then(|| event_unicode.to_string())
        }
        Ok(None) | Err(()) => None,
    }
}

pub(crate) fn is_printable(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|ch| !ch.is_control())
}

/// Retained source reference keeps the borrowed CFData returned by TISGetInputSourceProperty
/// alive. TIS APIs are main-thread-only in AppKit processes; all access is from the panel timer.
struct LayoutData {
    source: *mut c_void,
    data: *const c_void,
}

struct LayoutCache {
    current: Option<LayoutData>,
    ascii_capable: Option<LayoutData>,
}

unsafe impl Send for LayoutCache {}

static LAYOUT: OnceLock<Mutex<Option<LayoutCache>>> = OnceLock::new();

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
    static kTISNotifySelectedKeyboardInputSourceChanged: *const c_void;
    fn TISCopyCurrentKeyboardInputSource() -> *mut c_void;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> *mut c_void;
    fn TISGetInputSourceProperty(source: *mut c_void, property: *const c_void) -> *const c_void;
    fn LMGetKbdType() -> u8;
}

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn UCKeyTranslate(
        layout: *const c_void,
        virtual_key: u16,
        key_action: u16,
        modifier_state: u32,
        keyboard_type: u32,
        options: u32,
        dead_key_state: *mut u32,
        max_length: u32,
        actual_length: *mut u32,
        output: *mut u16,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
}

/// Refresh after the selected input source notification, and once during UI startup.
pub(crate) fn refresh_layout_cache() {
    if !crate::is_main_thread() {
        crate::debug_assert_main_thread();
        return;
    }
    unsafe {
        let current = layout_for_source(TISCopyCurrentKeyboardInputSource());
        let ascii_capable =
            layout_for_source(TISCopyCurrentASCIICapableKeyboardLayoutInputSource());
        let replacement = if current.is_some() || ascii_capable.is_some() {
            Some(LayoutCache {
                current,
                ascii_capable,
            })
        } else {
            None
        };
        let mut cache = LAYOUT.get_or_init(|| Mutex::new(None)).lock().unwrap();
        if let Some(mut old) = std::mem::replace(&mut *cache, replacement) {
            if let Some(layout) = old.current.take() {
                crate::ffi::CFRelease(layout.source);
            }
            if let Some(layout) = old.ascii_capable.take() {
                crate::ffi::CFRelease(layout.source);
            }
        }
    }
}

/// Borrowed notification name exported by HIToolbox for NSDistributedNotificationCenter.
pub(crate) unsafe fn layout_changed_notification() -> *const c_void {
    kTISNotifySelectedKeyboardInputSourceChanged
}

fn translate_unmodified(keycode: u16) -> Result<Option<String>, ()> {
    if !crate::is_main_thread() {
        crate::debug_assert_main_thread();
        return Err(());
    }
    let cache = LAYOUT.get_or_init(|| Mutex::new(None)).lock().unwrap();
    let current = cache
        .as_ref()
        .and_then(|cached| cached.current.as_ref())
        .map_or(Ok(None), |cached| unsafe {
            translate_cached_layout(cached, keycode)
        });
    let ascii_capable = cache
        .as_ref()
        .and_then(|cached| cached.ascii_capable.as_ref())
        .map_or(Ok(None), |cached| unsafe {
            translate_cached_layout(cached, keycode)
        });
    fallback_to_ascii_then_ansi(current, ascii_capable, keycode)
}

fn fallback_to_ascii_then_ansi(
    current: Result<Option<String>, ()>,
    ascii_capable: Result<Option<String>, ()>,
    keycode: u16,
) -> Result<Option<String>, ()> {
    match current {
        Ok(Some(text)) => Ok(Some(text)),
        Ok(None) | Err(()) => fallback_to_ansi(ascii_capable, keycode),
    }
}

fn fallback_to_ansi(
    translated: Result<Option<String>, ()>,
    keycode: u16,
) -> Result<Option<String>, ()> {
    match translated {
        Ok(Some(text)) => Ok(Some(text)),
        Ok(None) | Err(()) => Ok(ansi_key_glyph(keycode)),
    }
}

unsafe fn layout_for_source(source: *mut c_void) -> Option<LayoutData> {
    if source.is_null() {
        return None;
    }
    let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
    if data.is_null() {
        crate::ffi::CFRelease(source);
        None
    } else {
        Some(LayoutData { source, data })
    }
}

unsafe fn translate_cached_layout(cached: &LayoutData, keycode: u16) -> Result<Option<String>, ()> {
    let layout = CFDataGetBytePtr(cached.data);
    if layout.is_null() {
        Err(())
    } else {
        translate_with_layout(layout, keycode)
    }
}

unsafe fn translate_with_layout(layout: *const u8, keycode: u16) -> Result<Option<String>, ()> {
    let mut dead_key_state = 0u32;
    let mut actual_length = 0u32;
    let mut output = [0u16; 8];
    let status = UCKeyTranslate(
        layout as *const c_void,
        keycode,
        UC_KEY_ACTION_DOWN,
        0,
        LMGetKbdType() as u32,
        UC_KEY_TRANSLATE_NO_DEAD_KEYS,
        &mut dead_key_state,
        output.len() as u32,
        &mut actual_length,
        output.as_mut_ptr(),
    );
    if status != 0 || actual_length == 0 || actual_length as usize > output.len() {
        return Err(());
    }
    let text = String::from_utf16(&output[..actual_length as usize]).map_err(|_| ())?;
    if is_printable(&text) {
        Ok(Some(text))
    } else {
        Err(())
    }
}

fn ansi_key_glyph(keycode: u16) -> Option<String> {
    const ANSI_KEY_GLYPHS: &[(u16, &str)] = &[
        (0, "a"),
        (1, "s"),
        (2, "d"),
        (3, "f"),
        (4, "h"),
        (5, "g"),
        (6, "z"),
        (7, "x"),
        (8, "c"),
        (9, "v"),
        (11, "b"),
        (keyboard::VK_Q, "q"),
        (13, "w"),
        (14, "e"),
        (15, "r"),
        (16, "y"),
        (17, "t"),
        (18, "1"),
        (19, "2"),
        (20, "3"),
        (21, "4"),
        (22, "6"),
        (23, "5"),
        (24, "="),
        (25, "9"),
        (26, "7"),
        (27, "-"),
        (28, "8"),
        (29, "0"),
        (30, "]"),
        (31, "o"),
        (32, "u"),
        (33, "["),
        (34, "i"),
        (35, "p"),
        (37, "l"),
        (38, "j"),
        (39, "'"),
        (40, "k"),
        (41, ";"),
        (42, "\\"),
        (43, ","),
        (44, "/"),
        (45, "n"),
        (46, "m"),
        (47, "."),
        (50, "`"),
    ];
    ANSI_KEY_GLYPHS
        .iter()
        .find(|(code, _)| *code == keycode)
        .map(|(_, glyph)| (*glyph).to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        ansi_key_glyph, fallback_to_ascii_then_ansi, is_printable, modifier_glyphs, named_key,
        resolve_layout_glyph,
    };
    use crate::event_tap::keyboard;

    #[test]
    fn modifier_order_is_conventional() {
        assert_eq!(
            modifier_glyphs(
                keyboard::FLAG_COMMAND
                    | keyboard::FLAG_OPTION
                    | keyboard::FLAG_SHIFT
                    | keyboard::FLAG_CONTROL
            ),
            "⌘⌥⇧⌃"
        );
    }

    #[test]
    fn named_key_table_covers_navigation_and_function_keys() {
        assert_eq!(named_key(123).as_deref(), Some("←"));
        assert_eq!(named_key(90).as_deref(), Some("F20"));
        assert!(named_key(1).is_none());
    }

    #[test]
    fn printable_text_accepts_space_and_rejects_control_characters() {
        assert!(is_printable("a "));
        assert!(!is_printable("\n"));
        assert!(!is_printable(""));
    }

    #[test]
    fn event_unicode_is_used_only_when_layout_data_is_unavailable() {
        assert_eq!(resolve_layout_glyph("q", Ok(None), 0).as_deref(), Some("q"));
        assert_eq!(
            resolve_layout_glyph("Q", Ok(Some("q".into())), keyboard::FLAG_OPTION).as_deref(),
            Some("q")
        );
        assert_eq!(resolve_layout_glyph("Q", Err(()), 0), None);
        assert_eq!(resolve_layout_glyph("", Ok(None), 0), None);
    }

    #[test]
    fn modifier_chords_never_fall_back_to_modifier_affected_event_unicode() {
        for modifiers in [
            keyboard::FLAG_COMMAND,
            keyboard::FLAG_OPTION,
            keyboard::FLAG_SHIFT,
            keyboard::FLAG_CONTROL,
            keyboard::FLAG_CAPS_LOCK,
            keyboard::FLAG_FN,
        ] {
            assert_eq!(resolve_layout_glyph("œ", Ok(None), modifiers), None);
        }
        assert_eq!(
            resolve_layout_glyph("œ", Ok(Some("q".into())), keyboard::FLAG_OPTION).as_deref(),
            Some("q")
        );
        assert_eq!(resolve_layout_glyph("œ", Ok(None), 0).as_deref(), Some("œ"));
    }

    #[test]
    fn static_ansi_mapping_is_the_last_translation_fallback() {
        assert_eq!(ansi_key_glyph(keyboard::VK_Q).as_deref(), Some("q"));
        assert_eq!(ansi_key_glyph(0).as_deref(), Some("a"));
        assert_eq!(ansi_key_glyph(999), None);
        assert_eq!(
            fallback_to_ascii_then_ansi(Ok(Some("é".into())), Ok(Some("q".into())), keyboard::VK_Q),
            Ok(Some("é".into()))
        );
        assert_eq!(
            fallback_to_ascii_then_ansi(Ok(None), Ok(Some("q".into())), keyboard::VK_Q),
            Ok(Some("q".into()))
        );
        assert_eq!(fallback_to_ascii_then_ansi(Err(()), Err(()), 999), Ok(None));
    }
}
