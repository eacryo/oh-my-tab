//! 快捷键描述解析与显示。
//! 配置里按钮映射的值为 "cmd+shift+v" 这类字符串:修饰键名 + '+' + 键名。
//! 解析结果 = (虚拟键码, CGEventFlags 修饰位),供键盘模拟器合成事件。
//!
//! Shortcut-description parsing and display.
//! Button-mapping config values look like "cmd+shift+v": modifier names joined by '+' then a
//! key name. Parsing yields (virtual keycode, CGEventFlags modifier bits) for the key simulator.

use std::collections::HashMap;

/// CGEventFlags 修饰位(kCGEventFlagMask* 与 event_tap.rs 的常量一致)。
/// CGEventFlags modifier bits (kCGEventFlagMask*, matching event_tap.rs constants).
pub(crate) const FLAG_CMD: u32 = 0x0010_0000;
pub(crate) const FLAG_ALT: u32 = 0x0008_0000;
pub(crate) const FLAG_CTRL: u32 = 0x0004_0000;
pub(crate) const FLAG_SHIFT: u32 = 0x0002_0000;

/// 解析后的快捷键:(虚拟键码, 修饰位)。
/// A parsed shortcut: (virtual keycode, modifier bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shortcut {
    pub(crate) keycode: u16,
    pub(crate) flags: u32,
}

/// 单个字母/数字键名 -> 键码(ANSI 布局,键位序非字母序)。
/// Single letter/digit key name -> keycode (ANSI layout; physical order, not alphabetical).
fn ansi_keycode(name: &str) -> Option<u16> {
    let b = name.as_bytes();
    if b.len() != 1 {
        return None;
    }
    let c = b[0];
    match c {
        // QWERTY 键位序(ANSI):A=0x00 S=0x01 D=0x02 F=0x03 H=0x04 G=0x05 Z=0x06 X=0x07
        // C=0x08 V=0x09 B=0x0B Q=0x0C W=0x0D E=0x0E R=0x0F Y=0x10 T=0x11 U=0x20 I=0x22
        // O=0x1F P=0x23 L=0x25 J=0x26 K=0x28 N=0x2D M=0x2E
        b'a' => Some(0x00),
        b's' => Some(0x01),
        b'd' => Some(0x02),
        b'f' => Some(0x03),
        b'h' => Some(0x04),
        b'g' => Some(0x05),
        b'z' => Some(0x06),
        b'x' => Some(0x07),
        b'c' => Some(0x08),
        b'v' => Some(0x09),
        b'b' => Some(0x0B),
        b'q' => Some(0x0C),
        b'w' => Some(0x0D),
        b'e' => Some(0x0E),
        b'r' => Some(0x0F),
        b'y' => Some(0x10),
        b't' => Some(0x11),
        b'u' => Some(0x20),
        b'i' => Some(0x22),
        b'o' => Some(0x1F),
        b'p' => Some(0x23),
        b'l' => Some(0x25),
        b'j' => Some(0x26),
        b'k' => Some(0x28),
        b'n' => Some(0x2D),
        b'm' => Some(0x2E),
        b'0'..=b'9' => Some(match c {
            b'0' => 0x1D,
            b'1' => 0x12,
            b'2' => 0x13,
            b'3' => 0x14,
            b'4' => 0x15,
            b'5' => 0x17,
            b'6' => 0x16,
            b'7' => 0x1A,
            b'8' => 0x1C,
            b'9' => 0x19,
            _ => unreachable!(),
        }),
        _ => None,
    }
}

/// 特殊键名 -> 键码。
/// Special key names -> keycode.
fn special_keycode(name: &str) -> Option<u16> {
    Some(match name {
        "return" | "enter" => 0x24,
        "tab" => 0x30,
        "space" => 0x31,
        "delete" | "backspace" => 0x33,
        "escape" | "esc" => 0x35,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "home" => 115,
        "end" => 119,
        "pageup" | "pgup" => 116,
        "pagedown" | "pgdn" => 121,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        "f13" => 105,
        "f14" => 107,
        "f15" => 113,
        "f16" => 106,
        "f17" => 64,
        "f18" => 79,
        "f19" => 80,
        "f20" => 90,
        _ => return None,
    })
}

/// 把键名解析为键码(字母/数字/特殊键)。
/// Resolve a key name to a keycode (letter/digit/special).
fn keycode_for(name: &str) -> Option<u16> {
    ansi_keycode(name).or_else(|| special_keycode(name))
}

/// 解析 "cmd+shift+v" 这类描述。修饰键顺序任意、可省略;主键必须且只能一个。
/// 返回 Err(英文原因,供校验转成 i18n 消息)。
///
/// Parse a "cmd+shift+v"-style description. Modifiers may be in any order and are optional;
/// exactly one main key is required. Err carries an English reason for validation to wrap in
/// an i18n message.
pub(crate) fn parse_shortcut(desc: &str) -> Result<Shortcut, String> {
    let mut flags: u32 = 0;
    let mut main_key: Option<u16> = None;
    let mut unknown: Vec<String> = Vec::new();

    for raw in desc.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        let lower = part.to_ascii_lowercase();
        match lower.as_str() {
            "cmd" | "command" => flags |= FLAG_CMD,
            "alt" | "option" => flags |= FLAG_ALT,
            "ctrl" | "control" => flags |= FLAG_CTRL,
            "shift" => flags |= FLAG_SHIFT,
            _ => {
                if main_key.is_none() {
                    if let Some(kc) = keycode_for(&lower) {
                        main_key = Some(kc);
                        continue;
                    }
                }
                unknown.push(part.to_string());
            }
        }
    }

    if let Some(kc) = main_key {
        if unknown.is_empty() {
            return Ok(Shortcut { keycode: kc, flags });
        }
        Err(format!("unknown key(s): {}", unknown.join(", ")))
    } else {
        Err("no main key".into())
    }
}

/// 键码 -> 显示名(字母大写/数字/特殊键名)。
/// Keycode -> display name (capitalized letter/digit/special name).
fn key_name(keycode: u16) -> String {
    for (name, kc) in [
        ("a", 0x00),
        ("s", 0x01),
        ("d", 0x02),
        ("f", 0x03),
        ("h", 0x04),
        ("g", 0x05),
        ("z", 0x06),
        ("x", 0x07),
        ("c", 0x08),
        ("v", 0x09),
        ("b", 0x0B),
        ("q", 0x0C),
        ("w", 0x0D),
        ("e", 0x0E),
        ("r", 0x0F),
        ("y", 0x10),
        ("t", 0x11),
        ("u", 0x20),
        ("i", 0x22),
        ("o", 0x1F),
        ("p", 0x23),
        ("l", 0x25),
        ("j", 0x26),
        ("k", 0x28),
        ("n", 0x2D),
        ("m", 0x2E),
    ] {
        if kc == keycode {
            return name.to_uppercase();
        }
    }
    // 数字键的键码不连续,查表反向。
    // Digit keycodes are non-contiguous; reverse lookup.
    for (name, kc) in [
        ("0", 0x1D),
        ("1", 0x12),
        ("2", 0x13),
        ("3", 0x14),
        ("4", 0x15),
        ("5", 0x17),
        ("6", 0x16),
        ("7", 0x1A),
        ("8", 0x1C),
        ("9", 0x19),
    ] {
        if kc == keycode {
            return name.to_string();
        }
    }
    match keycode {
        0x24 => "Return".into(),
        0x30 => "Tab".into(),
        0x31 => "Space".into(),
        0x33 => "Delete".into(),
        0x35 => "Esc".into(),
        123 => "←".into(),
        124 => "→".into(),
        125 => "↓".into(),
        126 => "↑".into(),
        115 => "Home".into(),
        119 => "End".into(),
        116 => "Page Up".into(),
        121 => "Page Down".into(),
        k => {
            for (i, kc) in [
                122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79,
                80, 90,
            ]
            .iter()
            .enumerate()
            {
                if *kc == k {
                    return format!("F{}", i + 1);
                }
            }
            format!("key#{}", k)
        }
    }
}

/// 修饰位 -> 显示符号(⌘⇧⌥⌃,顺序固定 cmd shift alt ctrl)。
/// Modifier bits -> display symbols (⌘⇧⌥⌃, fixed order cmd shift alt ctrl).
pub(crate) fn modifier_display(flags: u32) -> String {
    let mut s = String::new();
    if flags & FLAG_CMD != 0 {
        s.push('⌘');
    }
    if flags & FLAG_SHIFT != 0 {
        s.push('⇧');
    }
    if flags & FLAG_ALT != 0 {
        s.push('⌥');
    }
    if flags & FLAG_CTRL != 0 {
        s.push('⌃');
    }
    s
}

/// 把配置里的快捷键描述解析并格式化成键帽样式(如 "cmd+shift+v" -> "⌘⇧V")。
/// 解析失败时原样返回(UI 里由校验报错,这里不 panic)。
///
/// Format a config shortcut description as keycap style ("cmd+shift+v" -> "⌘⇧V").
/// Returns the input untouched on parse failure (validation reports the error; no panic here).
pub(crate) fn display_shortcut(desc: &str) -> String {
    match parse_shortcut(desc) {
        Ok(s) => format!("{}{}", modifier_display(s.flags), key_name(s.keycode)),
        Err(_) => desc.to_string(),
    }
}

/// 键码 + 修饰位 -> 描述字符串(供录制后序列化进配置,如 ⌘⇧V 的事件 -> "cmd+shift+v")。
/// 修饰键按 cmd,shift,alt,ctrl 顺序输出。
///
/// Keycode + modifier bits -> description string (for serializing a recorded combo into the
/// config, e.g. a ⌘⇧V event -> "cmd+shift+v"). Modifiers serialize in cmd,shift,alt,ctrl order.
pub(crate) fn describe_shortcut(keycode: u16, flags: u32) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if flags & FLAG_CMD != 0 {
        parts.push("cmd");
    }
    if flags & FLAG_SHIFT != 0 {
        parts.push("shift");
    }
    if flags & FLAG_ALT != 0 {
        parts.push("alt");
    }
    if flags & FLAG_CTRL != 0 {
        parts.push("ctrl");
    }
    let key = key_name(keycode).to_ascii_lowercase();
    // 特殊键名小写后与解析表一致;单字符键直接小写。
    // Special-key names lowercase to match the parser; single chars just lowercase.
    let key = match key.as_str() {
        "return" | "tab" | "space" | "delete" | "esc" | "home" | "end" | "page up"
        | "page down" | "←" | "→" | "↓" | "↑" => {
            // 方向键等保持原样(描述表用小写英文名)。
            // Arrows etc. keep their own form; the parser table uses lowercase English names.
            match key.as_str() {
                "return" => "return",
                "tab" => "tab",
                "space" => "space",
                "delete" => "delete",
                "esc" => "escape",
                "home" => "home",
                "end" => "end",
                "page up" => "pageup",
                "page down" => "pagedown",
                "←" => "left",
                "→" => "right",
                "↓" => "down",
                "↑" => "up",
                _ => unreachable!(),
            }
        }
        k if k.starts_with('f') && k.len() > 1 && k[1..].chars().all(|c| c.is_ascii_digit()) => {
            // F 键:key_name 返回 "fN"(已小写)。
            // F keys: key_name already returns "fN" lowercased.
            k
        }
        k => k,
    };
    parts.push(key);
    parts.join("+")
}

/// 按钮号显示名(1-based 转 0-based 后映射到常见名字)。
/// Button-number display name (maps the 1-based number to a common name after 0-basing).
pub(crate) fn button_name(button: u32) -> String {
    match button {
        2 => "Middle".to_string(),
        3 => "Back".to_string(),
        4 => "Forward".to_string(),
        n => format!("Button {n}"),
    }
}

/// 校验按钮映射表:按钮号 >= 2 且是数字,快捷键可解析。
/// 返回错误列表(英文,调用方转 i18n)。
///
/// Validate a button-mapping table: button numbers are numeric and >= 2, shortcuts parse.
/// Returns a list of errors (English; the caller wraps them in i18n messages).
pub(crate) fn validate_mappings(mappings: &HashMap<String, String>, prefix: &str) -> Vec<String> {
    let mut errs = Vec::new();
    for (btn, desc) in mappings {
        match btn.parse::<u32>() {
            Ok(n) if n >= 2 => {}
            _ => errs.push(format!(
                "{prefix}.button_mappings[{btn}]: invalid button number"
            )),
        }
        if let Err(e) = parse_shortcut(desc) {
            errs.push(format!("{prefix}.button_mappings[{btn}]: {e}"));
        }
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_key() {
        let s = parse_shortcut("cmd+shift+v").unwrap();
        assert_eq!(s.keycode, 0x09); // V
        assert_eq!(s.flags, FLAG_CMD | FLAG_SHIFT);
    }

    #[test]
    fn parses_modifier_order_insensitive() {
        let a = parse_shortcut("alt+ctrl+1").unwrap();
        let b = parse_shortcut("ctrl+alt+1").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.keycode, 0x12); // '1'
    }

    #[test]
    fn parses_bare_key() {
        let s = parse_shortcut("f5").unwrap();
        assert_eq!(s.keycode, 96);
        assert_eq!(s.flags, 0);
    }

    #[test]
    fn rejects_unknown_key() {
        assert!(parse_shortcut("cmd+xyz").is_err());
        assert!(parse_shortcut("").is_err());
        assert!(parse_shortcut("cmd+").is_err());
    }

    #[test]
    fn display_roundtrip() {
        let desc = describe_shortcut(0x09, FLAG_CMD | FLAG_SHIFT);
        assert_eq!(desc, "cmd+shift+v");
        assert_eq!(display_shortcut(&desc), "⌘⇧V");
    }

    #[test]
    fn display_special_keys() {
        let desc = describe_shortcut(48, FLAG_ALT); // Tab + Option
        assert_eq!(desc, "alt+tab");
        assert_eq!(display_shortcut(&desc), "⌥Tab");
    }

    #[test]
    fn validate_buttons() {
        let mut m = HashMap::new();
        m.insert("3".into(), "cmd+c".into());
        m.insert("1".into(), "cmd+v".into()); // 左键不允许
        m.insert("4".into(), "badkey".into()); // 快捷键非法
        let errs = validate_mappings(&m, "mouse.profiles[0]");
        assert_eq!(errs.len(), 2);
    }
}
