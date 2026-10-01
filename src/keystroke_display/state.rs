//! Pure keystroke badge state machine. It receives virtual time and never logs content.

use std::time::{Duration, Instant};

use crate::event_tap::keyboard;

const SHORTCUT_MODIFIERS: u64 =
    keyboard::FLAG_COMMAND | keyboard::FLAG_OPTION | keyboard::FLAG_SHIFT | keyboard::FLAG_CONTROL;
const MODIFIER_ONLY_FADE: Duration = Duration::from_millis(600);
pub(super) const IDLE_FADE: Duration = Duration::from_millis(1800);
pub(crate) const BADGE_GAP: f64 = 6.0;
pub(crate) const PANEL_SIDE_PADDING: f64 = 12.0;
pub(crate) const BADGE_HORIZONTAL_PADDING: f64 = 28.0;

pub(crate) fn estimated_badge_width(text: &str, repeats: u32) -> f64 {
    let repeat_width = if repeats > 1 {
        22.0 + (repeats.ilog10() as f64 * 8.0)
    } else {
        0.0
    };
    text.chars().map(estimated_glyph_width).sum::<f64>() + BADGE_HORIZONTAL_PADDING + repeat_width
}

fn estimated_glyph_width(glyph: char) -> f64 {
    let value = glyph as u32;
    if is_zero_width_component(value) {
        0.0
    } else if is_emoji(value) {
        18.0
    } else if is_full_width(value) {
        16.0
    } else {
        10.0
    }
}

fn is_zero_width_component(value: u32) -> bool {
    matches!(
        value,
        0x0300..=0x036F
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x20D0..=0x20FF
            | 0x200D
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0xE0100..=0xE01EF
            | 0x1F3FB..=0x1F3FF
    )
}

fn is_emoji(value: u32) -> bool {
    matches!(value, 0x2600..=0x27BF | 0x1F000..=0x1FAFF)
}

fn is_full_width(value: u32) -> bool {
    matches!(
        value,
        0x1100..=0x11FF
            | 0x2E80..=0xA4CF
            | 0xAC00..=0xD7AF
            | 0xF900..=0xFAFF
            | 0xFE10..=0xFE6F
            | 0xFF01..=0xFF60
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x3FFFD
    )
}

pub(crate) fn estimated_stream_width(widths: impl IntoIterator<Item = f64>) -> f64 {
    let (total, count) = widths
        .into_iter()
        .fold((0.0, 0usize), |(total, count), width| {
            (total + width, count + 1)
        });
    total + count.saturating_sub(1) as f64 * BADGE_GAP + PANEL_SIDE_PADDING * 2.0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DisplayMode {
    All,
    Shortcuts,
    Commands,
}

impl DisplayMode {
    pub(crate) fn from_config(value: &str) -> Self {
        match value {
            "shortcuts" => Self::Shortcuts,
            "commands" => Self::Commands,
            _ => Self::All,
        }
    }

    pub(crate) fn accepts(self, flags: u64) -> bool {
        match self {
            Self::All => true,
            Self::Shortcuts => flags & SHORTCUT_MODIFIERS != 0,
            Self::Commands => flags & keyboard::FLAG_COMMAND != 0,
        }
    }

    fn visible_modifier_mask(self) -> u64 {
        match self {
            Self::All => crate::keystroke_display::mapping::modifier_mask(),
            Self::Shortcuts => SHORTCUT_MODIFIERS,
            Self::Commands => keyboard::FLAG_COMMAND,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BadgeKind {
    Modifier,
    ModifierReleased,
    Chord,
    Indicator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Badge {
    pub(crate) text: String,
    pub(crate) kind: BadgeKind,
    pub(crate) repeats: u32,
}

impl Badge {
    fn new(text: String, kind: BadgeKind) -> Self {
        Self {
            text,
            kind,
            repeats: 1,
        }
    }

    pub(crate) fn estimated_width(&self) -> f64 {
        estimated_badge_width(&self.text, self.repeats)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Input {
    KeyDown {
        keycode: u16,
        flags: u64,
        autorepeat: bool,
        unicode: String,
        glyph: KeyGlyph,
    },
    FlagsChanged {
        flags: u64,
        keycode: u16,
    },
    SecureActive(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyGlyph {
    /// No uchr layout is available, so event Unicode is the approved fallback.
    EventUnicode,
    Mapped(String),
    /// A layout exists but could not translate this key; do not use modified event Unicode.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LastKey {
    keycode: u16,
    modifiers: u64,
    badge_index: usize,
    press_count: u32,
    glyph: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateMachine {
    badges: Vec<Badge>,
    active_flags: u64,
    modifier_badge: Option<usize>,
    last_key: Option<LastKey>,
    deadline: Option<Instant>,
    secure: bool,
}

struct KeyInput<'a> {
    keycode: u16,
    flags: u64,
    unicode: &'a str,
    glyph: Option<&'a str>,
}

impl StateMachine {
    pub(crate) fn apply(&mut self, input: Input, mode: DisplayMode, now: Instant) -> bool {
        let before = self.clone();
        match input {
            Input::SecureActive(active) => self.set_secure(active),
            Input::FlagsChanged { flags, keycode } => {
                self.apply_flags_changed(flags, keycode, mode, now)
            }
            Input::KeyDown {
                keycode,
                flags,
                autorepeat: _,
                unicode,
                glyph,
            } => self.apply_key_down(
                KeyInput {
                    keycode,
                    flags,
                    unicode: &unicode,
                    glyph: match &glyph {
                        KeyGlyph::EventUnicode => Some(&unicode),
                        KeyGlyph::Mapped(text) => Some(text),
                        KeyGlyph::Unavailable => None,
                    },
                },
                mode,
                now,
            ),
        }
        *self != before
    }

    pub(crate) fn tick(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.deadline else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.badges.clear();
        self.modifier_badge = None;
        self.last_key = None;
        self.deadline = None;
        true
    }

    pub(crate) fn badges(&self) -> &[Badge] {
        &self.badges
    }

    #[cfg(test)]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn panel_visible(&self) -> bool {
        !self.badges.is_empty()
    }

    pub(crate) fn note_activity(&mut self, now: Instant) {
        if !self.secure && !self.badges.is_empty() {
            self.deadline = Some(now + IDLE_FADE);
        }
    }

    pub(crate) fn secure_paused(&self) -> bool {
        self.secure
    }

    pub(crate) fn trim_to_width(&mut self, max_width: f64) -> bool {
        let before = self.badges.clone();
        while self.badges.len() > 1 && self.stream_width() > max_width {
            self.badges.remove(0);
            self.rebase_indices_after_front_drop();
        }
        self.badges != before
    }

    fn stream_width(&self) -> f64 {
        estimated_stream_width(self.badges.iter().map(Badge::estimated_width))
    }

    fn rebase_indices_after_front_drop(&mut self) {
        self.modifier_badge = self.modifier_badge.and_then(|index| index.checked_sub(1));
        if let Some(last) = &mut self.last_key {
            if last.badge_index == 0 {
                self.last_key = None;
            } else {
                last.badge_index -= 1;
            }
        }
    }

    fn set_secure(&mut self, active: bool) {
        if self.secure == active {
            return;
        }
        self.secure = active;
        self.badges.clear();
        self.modifier_badge = None;
        self.last_key = None;
        self.deadline = None;
        if active {
            self.badges
                .push(Badge::new(String::new(), BadgeKind::Indicator));
        }
    }

    fn apply_flags_changed(&mut self, flags: u64, keycode: u16, mode: DisplayMode, now: Instant) {
        let old_flags = self.active_flags;
        self.active_flags = flags;
        if (old_flags ^ flags) & crate::keystroke_display::mapping::modifier_mask() != 0 {
            self.last_key = None;
        }
        if self.secure {
            return;
        }
        let visible_mask = mode.visible_modifier_mask();
        let added = (flags & !old_flags) & visible_mask;
        let new_visible_flags = flags & visible_mask;
        let is_caps_lock = keycode == keyboard::VK_CAPS_LOCK && added != 0;

        if new_visible_flags != 0 && mode.accepts(flags) {
            let text = crate::keystroke_display::mapping::modifier_glyphs(new_visible_flags);
            if !text.is_empty() {
                if let Some(index) = self.modifier_badge {
                    if let Some(badge) = self.badges.get_mut(index) {
                        if matches!(
                            badge.kind,
                            BadgeKind::Modifier | BadgeKind::ModifierReleased
                        ) {
                            badge.text = text;
                            badge.kind = BadgeKind::Modifier;
                        }
                    }
                } else {
                    self.modifier_badge = Some(self.push_badge(text, BadgeKind::Modifier));
                }
                if added != 0 {
                    self.last_key = None;
                }
                self.deadline = None;
            }
        }

        if is_caps_lock {
            if let Some(index) = self.modifier_badge {
                if let Some(badge) = self.badges.get(index) {
                    if badge.kind == BadgeKind::Modifier {
                        self.deadline = Some(now + MODIFIER_ONLY_FADE);
                    }
                }
            }
        } else if new_visible_flags == 0 {
            if let Some(index) = self.modifier_badge {
                if let Some(badge) = self.badges.get_mut(index) {
                    if badge.kind == BadgeKind::Modifier {
                        badge.kind = BadgeKind::ModifierReleased;
                    }
                    self.deadline = Some(now + MODIFIER_ONLY_FADE);
                }
            }
        } else if self.modifier_badge.is_some() {
            self.deadline = None;
        }
    }

    fn apply_key_down(&mut self, input: KeyInput<'_>, mode: DisplayMode, now: Instant) {
        let KeyInput {
            keycode,
            flags,
            unicode,
            glyph,
        } = input;
        self.active_flags = flags;
        if self.secure {
            return;
        }
        if !mode.accepts(flags) {
            self.last_key = None;
            return;
        }

        let actual_modifiers = flags & crate::keystroke_display::mapping::modifier_mask();
        if self.repeat_key_group(keycode, actual_modifiers, now) {
            return;
        }

        let visible_modifiers = flags & mode.visible_modifier_mask();
        if visible_modifiers != 0 {
            let Some(glyph) = glyph else {
                self.last_key = None;
                return;
            };
            let modifier_text =
                crate::keystroke_display::mapping::modifier_glyphs(visible_modifiers);
            let text = format!("{modifier_text}{glyph}");
            let group_glyph = text.clone();
            let index = if let Some(index) = self.modifier_badge.take() {
                if let Some(badge) = self.badges.get_mut(index) {
                    if matches!(
                        badge.kind,
                        BadgeKind::Modifier | BadgeKind::ModifierReleased
                    ) {
                        badge.text = text;
                        badge.kind = BadgeKind::Chord;
                        badge.repeats = 1;
                        index
                    } else {
                        self.push_badge(text, BadgeKind::Chord)
                    }
                } else {
                    self.push_badge(text, BadgeKind::Chord)
                }
            } else {
                self.push_badge(text, BadgeKind::Chord)
            };
            self.last_key = Some(LastKey {
                keycode,
                modifiers: actual_modifiers,
                badge_index: index,
                press_count: 1,
                glyph: group_glyph,
            });
            self.deadline = Some(now + IDLE_FADE);
            return;
        }

        self.modifier_badge = None;
        self.deadline = Some(now + IDLE_FADE);
        if keycode == keyboard::VK_SPACE {
            let Some(glyph) = glyph else {
                self.last_key = None;
                return;
            };
            self.start_badge_group(keycode, actual_modifiers, glyph, BadgeKind::Chord);
            return;
        }

        // Every key is its own badge: consecutive printable characters are never run together
        // into a shared text capsule.
        if crate::keystroke_display::mapping::is_printable(unicode) {
            let display_glyph = glyph.unwrap_or(unicode);
            self.start_badge_group(keycode, actual_modifiers, display_glyph, BadgeKind::Chord);
            return;
        }

        let Some(glyph) = glyph else {
            self.last_key = None;
            return;
        };

        self.start_badge_group(keycode, actual_modifiers, glyph, BadgeKind::Chord);
    }

    fn repeat_key_group(&mut self, keycode: u16, modifiers: u64, now: Instant) -> bool {
        let Some(last) = self.last_key.as_ref().cloned() else {
            return false;
        };
        if last.keycode != keycode || last.modifiers != modifiers {
            return false;
        }

        // The same key pressed repeatedly shows three separate badges, then collapses into one
        // with a count that keeps climbing on further presses.
        let next_count = last.press_count.saturating_add(1);
        if last.press_count < 3 {
            self.push_badge(last.glyph, BadgeKind::Chord);
        } else if last.press_count == 3 {
            let mut badge = Badge::new(last.glyph, BadgeKind::Chord);
            badge.repeats = next_count;
            self.badges.splice(
                last.badge_index..last.badge_index + 3,
                std::iter::once(badge),
            );
        } else if let Some(badge) = self.badges.get_mut(last.badge_index) {
            badge.repeats = next_count;
        }
        if let Some(group) = self.last_key.as_mut() {
            group.press_count = next_count;
        }
        self.deadline = Some(now + IDLE_FADE);
        true
    }

    fn start_badge_group(&mut self, keycode: u16, modifiers: u64, glyph: &str, kind: BadgeKind) {
        let badge_index = self.push_badge(glyph.to_string(), kind);
        self.last_key = Some(LastKey {
            keycode,
            modifiers,
            badge_index,
            press_count: 1,
            glyph: glyph.to_string(),
        });
    }

    fn push_badge(&mut self, text: String, kind: BadgeKind) -> usize {
        self.badges.push(Badge::new(text, kind));
        self.badges.len() - 1
    }
}

#[cfg(test)]
mod tests {
    use super::{
        estimated_badge_width, estimated_stream_width, BadgeKind, DisplayMode, Input, KeyGlyph,
        StateMachine, IDLE_FADE, MODIFIER_ONLY_FADE,
    };
    use crate::event_tap::keyboard;
    use std::time::{Duration, Instant};

    fn down(keycode: u16, flags: u64, unicode: &str) -> Input {
        down_with_repeat(keycode, flags, false, unicode)
    }

    fn down_with_repeat(keycode: u16, flags: u64, autorepeat: bool, unicode: &str) -> Input {
        Input::KeyDown {
            keycode,
            flags,
            autorepeat,
            unicode: unicode.to_string(),
            glyph: KeyGlyph::Mapped(match keycode {
                keyboard::VK_SPACE => "Space".to_string(),
                123 => "←".to_string(),
                12 => unicode.to_lowercase(),
                _ => unicode.to_string(),
            }),
        }
    }

    fn flags(flags: u64, keycode: u16) -> Input {
        Input::FlagsChanged { flags, keycode }
    }

    #[test]
    fn modifiers_merge_in_place_and_finalize_as_chord() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_COMMAND, 55), DisplayMode::All, now);
        state.apply(
            flags(keyboard::FLAG_COMMAND | keyboard::FLAG_SHIFT, 56),
            DisplayMode::All,
            now + Duration::from_millis(10),
        );
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].text, "⌘⇧");
        state.apply(
            down(12, keyboard::FLAG_COMMAND | keyboard::FLAG_SHIFT, "Q"),
            DisplayMode::All,
            now + Duration::from_millis(20),
        );
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].kind, BadgeKind::Chord);
        assert_eq!(state.badges()[0].text, "⌘⇧q");
        assert_eq!(state.deadline(), Some(now + Duration::from_millis(1820)));
    }

    #[test]
    fn failed_layout_translation_never_uses_event_unicode_for_a_chord() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_COMMAND, 55), DisplayMode::All, now);
        state.apply(
            Input::KeyDown {
                keycode: 12,
                flags: keyboard::FLAG_COMMAND,
                autorepeat: false,
                unicode: "Q".into(),
                glyph: KeyGlyph::Unavailable,
            },
            DisplayMode::All,
            now + Duration::from_millis(10),
        );
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].text, "⌘");
        assert_eq!(state.badges()[0].kind, BadgeKind::Modifier);
    }

    #[test]
    fn repeated_keys_show_three_instances_then_collapse_and_count_all_keydowns() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(0, 0, "a"), DisplayMode::All, now);
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].text, "a");
        state.apply(
            down(0, 0, "a"),
            DisplayMode::All,
            now + Duration::from_millis(1),
        );
        assert_eq!(state.badges().len(), 2);
        state.apply(
            down_with_repeat(0, 0, true, "a"),
            DisplayMode::All,
            now + Duration::from_millis(2),
        );
        assert_eq!(state.badges().len(), 3);
        assert!(state.badges().iter().all(|badge| badge.text == "a"));
        state.apply(
            down(0, 0, "a"),
            DisplayMode::All,
            now + Duration::from_millis(3),
        );
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].text, "a");
        assert_eq!(state.badges()[0].kind, BadgeKind::Chord);
        assert_eq!(state.badges()[0].repeats, 4);
        state.apply(
            down_with_repeat(0, 0, true, "a"),
            DisplayMode::All,
            now + Duration::from_millis(4),
        );
        assert_eq!(state.badges()[0].repeats, 5);
    }

    #[test]
    fn chords_use_the_same_four_press_collapse_threshold() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        for count in 1..=3 {
            state.apply(
                down_with_repeat(12, keyboard::FLAG_COMMAND, count == 2, "q"),
                DisplayMode::All,
                now + Duration::from_millis(count),
            );
            assert_eq!(state.badges().len(), count as usize);
            assert!(state.badges().iter().all(|badge| badge.repeats == 1));
        }
        state.apply(
            down(12, keyboard::FLAG_COMMAND, "q"),
            DisplayMode::All,
            now + Duration::from_millis(4),
        );
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].text, "⌘q");
        assert_eq!(state.badges()[0].repeats, 4);
        state.apply(
            down(12, keyboard::FLAG_COMMAND, "q"),
            DisplayMode::All,
            now + Duration::from_millis(5),
        );
        assert_eq!(state.badges()[0].repeats, 5);
    }

    #[test]
    fn each_printable_key_gets_its_own_badge() {
        // "hello": the two consecutive l's are separate presses, so they are two badges rather
        // than one merged capsule.
        let now = Instant::now();
        let mut state = StateMachine::default();
        let keys = [(4u16, "h"), (14, "e"), (37, "l"), (37, "l"), (31, "o")];
        for (offset, (keycode, text)) in keys.into_iter().enumerate() {
            state.apply(
                down(keycode, 0, text),
                DisplayMode::All,
                now + Duration::from_millis(offset as u64),
            );
        }
        let texts: Vec<&str> = state
            .badges()
            .iter()
            .map(|badge| badge.text.as_str())
            .collect();
        assert_eq!(texts, ["h", "e", "l", "l", "o"]);
    }

    #[test]
    fn a_different_key_resets_the_repeat_group() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        for offset in 0..2 {
            state.apply(
                down(0, 0, "a"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        state.apply(
            down(11, 0, "b"),
            DisplayMode::All,
            now + Duration::from_millis(2),
        );
        for offset in 3..7 {
            state.apply(
                down(0, 0, "a"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        let badges: Vec<(&str, u32)> = state
            .badges()
            .iter()
            .map(|badge| (badge.text.as_str(), badge.repeats))
            .collect();
        assert_eq!(badges, [("a", 1), ("a", 1), ("b", 1), ("a", 4)]);
    }

    #[test]
    fn modifier_change_secure_pause_and_hide_reset_repeat_groups() {
        let now = Instant::now();
        let mut modifier_change = StateMachine::default();
        for offset in 0..3 {
            modifier_change.apply(
                down(12, keyboard::FLAG_COMMAND, "q"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        modifier_change.apply(
            flags(keyboard::FLAG_COMMAND | keyboard::FLAG_OPTION, 58),
            DisplayMode::All,
            now + Duration::from_millis(3),
        );
        modifier_change.apply(
            down(12, keyboard::FLAG_COMMAND | keyboard::FLAG_OPTION, "q"),
            DisplayMode::All,
            now + Duration::from_millis(4),
        );
        assert_eq!(modifier_change.badges()[0].repeats, 1);
        assert_eq!(modifier_change.badges()[1].repeats, 1);
        assert_eq!(modifier_change.badges()[3].text, "⌘⌥q");
        assert_eq!(modifier_change.badges()[3].repeats, 1);

        let mut changed_chord = StateMachine::default();
        for offset in 0..3 {
            changed_chord.apply(
                down(12, keyboard::FLAG_COMMAND, "q"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        changed_chord.apply(
            down(12, keyboard::FLAG_COMMAND | keyboard::FLAG_OPTION, "q"),
            DisplayMode::All,
            now + Duration::from_millis(3),
        );
        assert_eq!(changed_chord.badges().len(), 4);
        assert_eq!(changed_chord.badges()[3].text, "⌘⌥q");
        assert_eq!(changed_chord.badges()[3].repeats, 1);

        let mut paused = StateMachine::default();
        for offset in 0..3 {
            paused.apply(
                down(0, 0, "a"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        paused.apply(Input::SecureActive(true), DisplayMode::All, now);
        paused.apply(Input::SecureActive(false), DisplayMode::All, now);
        paused.apply(down(0, 0, "a"), DisplayMode::All, now);
        assert_eq!(paused.badges().len(), 1);
        assert_eq!(paused.badges()[0].text, "a");
        assert_eq!(paused.badges()[0].repeats, 1);

        let mut hidden = StateMachine::default();
        for offset in 0..3 {
            hidden.apply(
                down(0, 0, "a"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        assert!(hidden.tick(now + IDLE_FADE + Duration::from_millis(2)));
        hidden.apply(
            down(0, 0, "a"),
            DisplayMode::All,
            now + IDLE_FADE + Duration::from_millis(3),
        );
        assert_eq!(hidden.badges().len(), 1);
        assert_eq!(hidden.badges()[0].text, "a");
        assert_eq!(hidden.badges()[0].repeats, 1);
    }

    #[test]
    fn many_distinct_keys_never_merge_into_one_badge() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        for keycode in 0..42u16 {
            // Distinct keycodes avoid repeat collapsing, so every press must add its own badge.
            state.apply(down(keycode, 0, "a"), DisplayMode::All, now);
        }
        assert_eq!(state.badges().len(), 42);
        assert!(state.badges().iter().all(|badge| badge.text == "a"));
    }

    #[test]
    fn shared_width_estimates_include_repeats_gap_and_padding() {
        assert_eq!(estimated_badge_width("abc", 1), 58.0);
        assert_eq!(estimated_badge_width("abc", 10), 88.0);
        assert_eq!(estimated_stream_width([58.0, 88.0]), 176.0);
        assert_eq!(estimated_badge_width("中文", 1), 60.0);
        assert_eq!(estimated_badge_width("🙂", 1), 46.0);
    }

    #[test]
    fn width_limit_keeps_the_current_oversized_badge_intact() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(0, 0, "a"), DisplayMode::All, now);
        let text = state.badges()[0].text.clone();
        assert!(!state.trim_to_width(1.0));
        assert_eq!(state.badges()[0].text, text);
    }

    #[test]
    fn space_is_its_own_named_key() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(0, 0, "h"), DisplayMode::All, now);
        state.apply(down(keyboard::VK_SPACE, 0, " "), DisplayMode::All, now);
        state.apply(down(1, 0, "i"), DisplayMode::All, now);
        let texts: Vec<&str> = state
            .badges()
            .iter()
            .map(|badge| badge.text.as_str())
            .collect();
        assert_eq!(texts, ["h", "Space", "i"]);
    }

    #[test]
    fn modifier_only_badge_fades_six_hundred_ms_after_release() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_OPTION, 58), DisplayMode::All, now);
        state.apply(
            flags(0, 58),
            DisplayMode::All,
            now + Duration::from_millis(5),
        );
        assert_eq!(state.deadline(), Some(now + Duration::from_millis(605)));
        assert!(!state.tick(now + MODIFIER_ONLY_FADE));
        assert!(state.tick(now + Duration::from_millis(605)));
        assert!(!state.panel_visible());
    }

    #[test]
    fn modifier_badge_tracks_held_modifiers_and_loses_accent_on_release() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_COMMAND, 55), DisplayMode::All, now);
        state.apply(
            flags(keyboard::FLAG_COMMAND | keyboard::FLAG_SHIFT, 56),
            DisplayMode::All,
            now + Duration::from_millis(10),
        );
        assert_eq!(state.badges()[0].text, "⌘⇧");
        assert_eq!(state.badges()[0].kind, BadgeKind::Modifier);

        state.apply(
            flags(keyboard::FLAG_COMMAND, 56),
            DisplayMode::All,
            now + Duration::from_millis(20),
        );
        assert_eq!(state.badges()[0].text, "⌘");
        assert_eq!(state.badges()[0].kind, BadgeKind::Modifier);

        state.apply(
            flags(0, 55),
            DisplayMode::All,
            now + Duration::from_millis(30),
        );
        assert_eq!(state.badges()[0].text, "⌘");
        assert_eq!(state.badges()[0].kind, BadgeKind::ModifierReleased);
        assert_eq!(state.deadline(), Some(now + Duration::from_millis(630)));
    }

    #[test]
    fn mode_filters_only_at_event_time() {
        let now = Instant::now();
        let mut shortcuts = StateMachine::default();
        shortcuts.apply(down(0, 0, "a"), DisplayMode::Shortcuts, now);
        shortcuts.apply(
            down(12, keyboard::FLAG_OPTION, "q"),
            DisplayMode::Shortcuts,
            now,
        );
        assert_eq!(shortcuts.badges().len(), 1);
        let mut commands = StateMachine::default();
        commands.apply(
            down(12, keyboard::FLAG_OPTION, "q"),
            DisplayMode::Commands,
            now,
        );
        commands.apply(
            down(12, keyboard::FLAG_COMMAND, "q"),
            DisplayMode::Commands,
            now,
        );
        assert_eq!(commands.badges().len(), 1);
        assert_eq!(commands.badges()[0].text, "⌘q");
    }

    #[test]
    fn secure_input_replaces_content_and_suppresses_keys_until_resume() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(0, 0, "secret"), DisplayMode::All, now);
        state.apply(Input::SecureActive(true), DisplayMode::All, now);
        state.apply(down(0, 0, "x"), DisplayMode::All, now);
        assert!(state.secure_paused());
        assert_eq!(state.badges().len(), 1);
        assert_eq!(state.badges()[0].kind, BadgeKind::Indicator);
        state.apply(Input::SecureActive(false), DisplayMode::All, now);
        assert!(!state.panel_visible());
        assert!(!state.secure_paused());
    }

    #[test]
    fn deadline_math_uses_virtual_time() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(123, 0, ""), DisplayMode::All, now);
        assert_eq!(state.deadline(), Some(now + IDLE_FADE));
        assert!(!state.tick(now + IDLE_FADE - Duration::from_millis(1)));
        assert!(state.tick(now + IDLE_FADE));
    }

    #[test]
    fn panel_activity_extends_idle_deadline_with_virtual_time() {
        let now = Instant::now();
        let activity = now + Duration::from_millis(900);
        let mut state = StateMachine::default();
        state.apply(down(123, 0, ""), DisplayMode::All, now);
        state.note_activity(activity);
        assert_eq!(state.deadline(), Some(activity + IDLE_FADE));
        assert!(!state.tick(now + IDLE_FADE));
        assert!(state.panel_visible());
        assert!(state.tick(activity + IDLE_FADE));
        assert!(!state.panel_visible());
    }
}
