//! Pure keystroke badge state machine. It receives virtual time and never logs content.

use std::time::{Duration, Instant};

use crate::event_tap::keyboard;

const SHORTCUT_MODIFIERS: u64 =
    keyboard::FLAG_COMMAND | keyboard::FLAG_OPTION | keyboard::FLAG_SHIFT | keyboard::FLAG_CONTROL;
const MODIFIER_ONLY_FADE: Duration = Duration::from_millis(600);
pub(super) const IDLE_FADE: Duration = Duration::from_millis(1800);
pub(crate) const BADGE_GAP: f64 = 6.0;
/// Height of one badge row when keys run horizontally, and of a single keycap in a column.
pub(crate) const BADGE_H: f64 = 34.0;
pub(crate) const PANEL_SIDE_PADDING: f64 = 12.0;
/// Horizontal padding inside a lone keycap. 8pt per side: keycap-tight rather than
/// capsule-roomy, which keeps the column rail narrow ("Paused", the widest shipped keycap
/// text, measures 63.5pt at this padding).
pub(crate) const BADGE_HORIZONTAL_PADDING: f64 = 16.0;
/// Minimum width for a horizontal lone keycap, so one-character keys have the same visual
/// weight as modifier cells (a Command cell is about 32pt including its insets).
pub(crate) const BADGE_MIN_WIDTH: f64 = 32.0;
/// Horizontal padding on each side of a multi-key badge container.
pub(crate) const BADGE_CONTAINER_PADDING_X: f64 = 4.0;
/// Horizontal padding on each side of one keycap cell inside a badge container.
pub(crate) const BADGE_CELL_PADDING_X: f64 = 7.0;
/// Gap between two keycap cells inside a badge container.
pub(crate) const BADGE_CELL_GAP: f64 = 4.0;
/// Vertical inset of a keycap cell from its container's top and bottom edges.
pub(crate) const BADGE_CELL_INSET_Y: f64 = 4.0;
/// Padding on each side of a badge container along the stacking axis (the mirror of
/// `BADGE_CONTAINER_PADDING_X` when keys are stacked instead of laid in a row).
pub(crate) const BADGE_CONTAINER_PADDING_Y: f64 = 4.0;
/// Height of the "×N" repeat suffix when it takes its own row under a stacked chord.
pub(crate) const BADGE_REPEAT_SUFFIX_H: f64 = 16.0;
/// A merge count stops climbing here: further autorepeats keep merging but the displayed
/// number freezes, so the widest possible suffix stays two digits wide.
pub(crate) const MAX_REPEAT_COUNT: u32 = 99;
/// The one fixed width EVERY keycap renders at in a column -- lone keycaps and split chord
/// cells alike (a column has no trays) -- so the rail and the panel behind it never change
/// as keys come and go. Sized to hold every shipped locale's widest keycap text without a
/// merge count ("Paused" measures 63.5pt at the 14pt badge font and the 16pt horizontal
/// padding; in a column the count renders on its own row below the glyph). The
/// page-up/down legends are short ("Pg Dn" / "下页") so no key name needs more width than
/// the common keys do. Only a column uses the shared width -- a row keeps content-sized
/// keycaps.
pub(crate) const KEYCAP_RAIL_W: f64 = 64.0;

/// The stream-axis length a merged keycap's own suffix row adds below its glyph in a column:
/// the gap plus the suffix row's height, or nothing when the key was pressed once.
pub(crate) fn repeat_suffix_row(repeats: u32) -> f64 {
    if repeats > 1 {
        BADGE_CELL_GAP + BADGE_REPEAT_SUFFIX_H
    } else {
        0.0
    }
}

/// Estimate a column chord after it is split into standalone keycaps, matching the panel's
/// per-badge stream spacing and the repeat suffix on the final keycap.
fn estimated_split_chord_height(cell_count: usize, repeats: u32) -> f64 {
    cell_count as f64 * BADGE_H
        + cell_count.saturating_sub(1) as f64 * BADGE_GAP
        + repeat_suffix_row(repeats)
}

/// The badges a COLUMN actually draws. Every keycap is independent: a chord is split into one
/// lone badge per cell (modifiers keep their accent tint), stacked in the stream like any
/// single key -- no grouping tray around them. The chord's repeat count rides on the LAST
/// cell, whose keycap then grows its own suffix row. Non-chord badges pass through
/// unchanged. Pure so the split is unit-assertable; a row keeps chords as trayed cells.
pub(crate) fn column_display_badges(badges: &[Badge]) -> Vec<Badge> {
    let mut out = Vec::with_capacity(badges.len());
    for badge in badges {
        if badge.cells.len() > 1 {
            let last = badge.cells.len().saturating_sub(1);
            for (index, cell) in badge.cells.iter().enumerate() {
                out.push(Badge {
                    text: cell.text().to_string(),
                    kind: if cell.is_accented_modifier() {
                        BadgeKind::Modifier
                    } else if cell.is_modifier() {
                        BadgeKind::ModifierReleased
                    } else {
                        BadgeKind::Chord
                    },
                    repeats: if index == last { badge.repeats } else { 1 },
                    cells: Vec::new(),
                });
            }
        } else {
            out.push(badge.clone());
        }
    }
    out
}

/// How the keys are laid out, decided by the screen edge the panel sits on. A panel against a
/// horizontal edge (`top`/`bottom`) runs its keys across in a row; one against a vertical edge
/// (`left`/`right`) stacks them in a column, because the strip is tall and narrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Orientation {
    Horizontal,
    Vertical,
}

impl Orientation {
    /// Resolve from `keystroke_display.initial_position`. An unknown value is horizontal, which
    /// is the shape every config predating this setting already had.
    pub(crate) fn from_initial_position(initial_position: &str) -> Self {
        match initial_position {
            "left" | "right" => Self::Vertical,
            _ => Self::Horizontal,
        }
    }

    pub(crate) fn is_vertical(self) -> bool {
        matches!(self, Self::Vertical)
    }
}

impl Default for Orientation {
    /// The shape every config predating `initial_position` already had.
    fn default() -> Self {
        Self::Horizontal
    }
}

pub(crate) fn estimated_badge_width(text: &str, repeats: u32) -> f64 {
    let repeat_width = if repeats > 1 {
        22.0 + (repeats.ilog10() as f64 * 8.0)
    } else {
        0.0
    };
    (text.chars().map(estimated_glyph_width).sum::<f64>() + BADGE_HORIZONTAL_PADDING + repeat_width)
        .max(BADGE_MIN_WIDTH)
}

/// Estimated width of a multi-key badge: a container holding one padded keycap cell per key,
/// with the repeat suffix after the last cell.
pub(crate) fn estimated_cells_width(cells: &[BadgeCell], repeats: u32) -> f64 {
    let mut width = BADGE_CONTAINER_PADDING_X * 2.0;
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            width += BADGE_CELL_GAP;
        }
        width += cell.text().chars().map(estimated_glyph_width).sum::<f64>()
            + BADGE_CELL_PADDING_X * 2.0;
    }
    if repeats > 1 {
        width += BADGE_CELL_GAP
            + format!("×{repeats}")
                .chars()
                .map(estimated_glyph_width)
                .sum::<f64>();
    }
    width
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

    /// The modifiers a key event may show as held. Excludes `FLAG_FN`, which the system sets on
    /// every function-key event regardless of whether fn is down.
    fn visible_modifier_mask(self) -> u64 {
        match self {
            Self::All => crate::keystroke_display::mapping::MODIFIER_MASK,
            Self::Shortcuts => SHORTCUT_MODIFIERS,
            Self::Commands => keyboard::FLAG_COMMAND,
        }
    }

    /// The modifiers a modifier event may show as held. Only here is `FLAG_FN` meaningful.
    fn visible_modifier_event_mask(self) -> u64 {
        match self {
            Self::All => crate::keystroke_display::mapping::modifier_mask(),
            Self::Shortcuts => SHORTCUT_MODIFIERS | keyboard::FLAG_FN,
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

/// One key inside a multi-key badge. Modifiers and the key they combine with are separated so
/// the panel can tint modifiers apart from the key, the way a keycap HUD does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BadgeCell {
    Modifier(String),
    ReleasedModifier(String),
    Key(String),
}

impl BadgeCell {
    pub(crate) fn text(&self) -> &str {
        match self {
            BadgeCell::Modifier(text)
            | BadgeCell::ReleasedModifier(text)
            | BadgeCell::Key(text) => text,
        }
    }

    pub(crate) fn is_modifier(&self) -> bool {
        matches!(
            self,
            BadgeCell::Modifier(_) | BadgeCell::ReleasedModifier(_)
        )
    }

    pub(crate) fn is_accented_modifier(&self) -> bool {
        matches!(self, BadgeCell::Modifier(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Badge {
    pub(crate) text: String,
    pub(crate) kind: BadgeKind,
    pub(crate) repeats: u32,
    /// The individual keys inside this badge: empty or single-element badges draw one label from
    /// `text`, while two or more draw one keycap cell per entry.
    pub(crate) cells: Vec<BadgeCell>,
}

impl Badge {
    fn new(text: String, kind: BadgeKind) -> Self {
        Self {
            text,
            kind,
            repeats: 1,
            cells: Vec::new(),
        }
    }

    fn with_cells(kind: BadgeKind, cells: Vec<BadgeCell>) -> Self {
        let text = cells.iter().map(|cell| cell.text()).collect();
        Self {
            text,
            kind,
            repeats: 1,
            cells,
        }
    }

    pub(crate) fn estimated_width(&self) -> f64 {
        if self.cells.len() > 1 {
            estimated_cells_width(&self.cells, self.repeats)
        } else {
            estimated_badge_width(&self.text, self.repeats)
        }
    }

    /// This badge's extent along the stream axis: its width when keys run in a row, its height
    /// when they stack in a column. A lone keycap is the same size either way, but its extent
    /// still swaps — a row advances across its width, a column down its height — so the trim cap
    /// measures the same axis the panel lays out. In a column a merged keycap grows DOWN the
    /// stream axis instead of widening: the count takes its own row below the glyph, mirroring
    /// a chord's suffix row.
    pub(crate) fn estimated_extent(&self, orientation: Orientation) -> f64 {
        match (orientation.is_vertical(), self.cells.len() > 1) {
            (true, true) => estimated_split_chord_height(self.cells.len(), self.repeats),
            (true, false) => BADGE_H + repeat_suffix_row(self.repeats),
            (false, _) => self.estimated_width(),
        }
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
    cells: Vec<BadgeCell>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StateMachine {
    badges: Vec<Badge>,
    active_flags: u64,
    modifier_badge: Option<usize>,
    last_key: Option<LastKey>,
    deadline: Option<Instant>,
    /// Set once the stream outgrows the panel's width cap. The panel then stays at the cap for
    /// the rest of the session, so old keys being pushed out does not make the bar breathe.
    capped: bool,
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

    /// Clear the whole stream once its idle deadline passes.
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
        self.capped = false;
        true
    }

    pub(crate) fn badges(&self) -> &[Badge] {
        &self.badges
    }

    #[cfg(test)]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Whether the stream has filled the panel's width cap this session.
    pub(crate) fn capped(&self) -> bool {
        self.capped
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

    /// Drop the oldest keys until the stream fits the panel's cap. The cap is measured along the
    /// stream axis, so a column trims against its height while a row trims against its width.
    pub(crate) fn trim_to_extent(&mut self, max_extent: f64, orientation: Orientation) -> bool {
        let before = self.badges.clone();
        while self.badges.len() > 1 && self.stream_extent(orientation) > max_extent {
            self.badges.remove(0);
            self.rebase_indices_after_front_drop();
            // Once the cap is reached, the panel pins to it for the rest of the session so the
            // window rolling (old keys pushed out by new ones) never changes the bar's length.
            self.capped = true;
        }
        self.badges != before
    }

    fn stream_extent(&self, orientation: Orientation) -> f64 {
        estimated_stream_width(
            self.badges
                .iter()
                .map(|badge| badge.estimated_extent(orientation)),
        )
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
        self.capped = false;
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
            self.release_chord_modifiers(flags, mode);
        }
        if self.secure {
            return;
        }
        let visible_mask = mode.visible_modifier_event_mask();
        let added = (flags & !old_flags) & visible_mask;
        let new_visible_flags = flags & visible_mask;
        let is_caps_lock = keycode == keyboard::VK_CAPS_LOCK && added != 0;

        if new_visible_flags != 0 && mode.accepts(flags) {
            let cells: Vec<BadgeCell> =
                crate::keystroke_display::mapping::modifier_event_cells(new_visible_flags)
                    .into_iter()
                    .map(BadgeCell::Modifier)
                    .collect();
            if !cells.is_empty() {
                if let Some(index) = self.modifier_badge {
                    if let Some(badge) = self.badges.get_mut(index) {
                        if matches!(
                            badge.kind,
                            BadgeKind::Modifier | BadgeKind::ModifierReleased
                        ) {
                            badge.text = cells.iter().map(|cell| cell.text()).collect();
                            badge.kind = BadgeKind::Modifier;
                            badge.cells = cells;
                        }
                    }
                } else {
                    self.modifier_badge =
                        Some(self.push_badge(Badge::with_cells(BadgeKind::Modifier, cells)));
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

    fn release_chord_modifiers(&mut self, flags: u64, mode: DisplayMode) {
        if self.secure {
            return;
        }
        let active: Vec<String> = crate::keystroke_display::mapping::modifier_event_cells(
            flags & mode.visible_modifier_event_mask(),
        );
        for badge in &mut self.badges {
            if badge.kind != BadgeKind::Chord {
                continue;
            }
            for cell in &mut badge.cells {
                if let BadgeCell::Modifier(text) = cell {
                    if !active.iter().any(|held| held == text) {
                        *cell = BadgeCell::ReleasedModifier(text.clone());
                    }
                }
            }
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

        // A key event's held modifiers exclude FLAG_FN: the system sets that bit on every
        // function-key event, so including it made a bare arrow key look like "fn ←".
        let actual_modifiers = flags & crate::keystroke_display::mapping::MODIFIER_MASK;
        if self.repeat_key_group(keycode, actual_modifiers, now) {
            return;
        }

        let visible_modifiers = flags & mode.visible_modifier_mask();
        if visible_modifiers != 0 {
            let Some(glyph) = glyph else {
                self.last_key = None;
                return;
            };
            // One cell per key: the held modifiers, then the key they combine with.
            let mut cells: Vec<BadgeCell> =
                crate::keystroke_display::mapping::modifier_cells(visible_modifiers)
                    .into_iter()
                    .map(BadgeCell::Modifier)
                    .collect();
            cells.push(BadgeCell::Key(glyph.to_string()));
            let index = if let Some(index) = self.modifier_badge.take() {
                if let Some(badge) = self.badges.get_mut(index) {
                    if matches!(
                        badge.kind,
                        BadgeKind::Modifier | BadgeKind::ModifierReleased
                    ) {
                        badge.text = cells.iter().map(|cell| cell.text()).collect();
                        badge.kind = BadgeKind::Chord;
                        badge.repeats = 1;
                        badge.cells = cells.clone();
                        index
                    } else {
                        self.push_badge(Badge::with_cells(BadgeKind::Chord, cells.clone()))
                    }
                } else {
                    self.push_badge(Badge::with_cells(BadgeKind::Chord, cells.clone()))
                }
            } else {
                self.push_badge(Badge::with_cells(BadgeKind::Chord, cells.clone()))
            };
            self.last_key = Some(LastKey {
                keycode,
                modifiers: actual_modifiers,
                badge_index: index,
                press_count: 1,
                glyph: cells.iter().map(|cell| cell.text()).collect(),
                cells,
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
        // with a count that keeps climbing on further presses -- freezing at
        // MAX_REPEAT_COUNT, past which further repeats merge without changing the number.
        let next_count = last.press_count.saturating_add(1).min(MAX_REPEAT_COUNT);
        if last.press_count < 3 {
            self.push_badge(repeat_badge(&last));
        } else if last.press_count == 3 {
            let mut badge = repeat_badge(&last);
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
        let badge_index = self.push_badge(Badge::new(glyph.to_string(), kind));
        self.last_key = Some(LastKey {
            keycode,
            modifiers,
            badge_index,
            press_count: 1,
            glyph: glyph.to_string(),
            cells: Vec::new(),
        });
    }

    fn push_badge(&mut self, badge: Badge) -> usize {
        self.badges.push(badge);
        self.badges.len() - 1
    }
}

/// Rebuild the badge for a repeated key, preserving a chord's cell split.
fn repeat_badge(last: &LastKey) -> Badge {
    if last.cells.is_empty() {
        Badge::new(last.glyph.clone(), BadgeKind::Chord)
    } else {
        Badge::with_cells(BadgeKind::Chord, last.cells.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        estimated_badge_width, estimated_cells_width, estimated_stream_width, repeat_suffix_row,
        Badge, BadgeCell, BadgeKind, DisplayMode, Input, KeyGlyph, Orientation, StateMachine,
        BADGE_H, BADGE_MIN_WIDTH, IDLE_FADE, MAX_REPEAT_COUNT, MODIFIER_ONLY_FADE,
    };
    use crate::event_tap::keyboard;
    use std::time::{Duration, Instant};

    /// A merged lone keycap in a column grows DOWN the stream axis (its own suffix row below
    /// the glyph), never wider; unmerged it stays one cap tall. A row keeps the count inline
    /// in the text, so there it is the width estimate that grows.
    #[test]
    fn merged_lone_keycap_extent_grows_down_not_wide() {
        let mut badge = Badge::new("A".to_string(), BadgeKind::Chord);
        assert_eq!(badge.estimated_extent(Orientation::Vertical), BADGE_H);
        let unmerged_width = badge.estimated_width();
        badge.repeats = 7;
        assert_eq!(
            badge.estimated_extent(Orientation::Vertical),
            BADGE_H + repeat_suffix_row(7)
        );
        assert!(badge.estimated_width() > unmerged_width);
    }

    /// A column splits every chord into independent lone keycaps -- one per cell, modifiers
    /// keeping the accent tint, no tray -- and the chord's repeat count rides on the last
    /// cell. Non-chord badges pass through unchanged.
    #[test]
    fn column_display_badges_split_chords_into_lone_keycaps() {
        use super::column_display_badges;
        let lone = Badge::new("A".to_string(), BadgeKind::Chord);
        let mut chord = Badge::with_cells(
            BadgeKind::Chord,
            vec![
                BadgeCell::Modifier("⌘".to_string()),
                BadgeCell::Key("C".to_string()),
            ],
        );
        chord.repeats = 5;
        let split = column_display_badges(&[lone.clone(), chord]);
        assert_eq!(split.len(), 3);
        assert_eq!(split[0].text, "A");
        assert_eq!(split[1].text, "⌘");
        assert_eq!(split[1].kind, BadgeKind::Modifier);
        assert_eq!(split[1].repeats, 1);
        assert_eq!(split[2].text, "C");
        assert_eq!(split[2].kind, BadgeKind::Chord);
        assert_eq!(split[2].repeats, 5);
        assert!(split.iter().all(|badge| badge.cells.is_empty()));
        // A stream with no chords is unchanged.
        let unchanged = column_display_badges(std::slice::from_ref(&lone));
        assert_eq!(unchanged, vec![lone]);
    }

    /// The merge count freezes at MAX_REPEAT_COUNT: past it, further autorepeats keep merging
    /// without changing the number, so the widest suffix stays two digits wide.
    #[test]
    fn repeat_count_freezes_at_the_cap() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        // Cross the 3-press threshold into merged form, then autorepeat far past the cap.
        for i in 0..4u64 {
            state.apply(
                down(0, 0, "a"),
                DisplayMode::All,
                now + Duration::from_millis(i),
            );
        }
        for i in 4..(MAX_REPEAT_COUNT as u64 + 40) {
            state.apply(
                down_with_repeat(0, 0, true, "a"),
                DisplayMode::All,
                now + Duration::from_millis(i),
            );
        }
        assert_eq!(state.badges()[0].repeats, MAX_REPEAT_COUNT);
    }

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
    fn releasing_command_neutralizes_its_chord_keycap() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_COMMAND, 55), DisplayMode::All, now);
        state.apply(
            down(48, keyboard::FLAG_COMMAND, "⇥"),
            DisplayMode::All,
            now + Duration::from_millis(10),
        );
        assert!(state.badges()[0].cells[0].is_accented_modifier());

        state.apply(
            flags(0, 55),
            DisplayMode::All,
            now + Duration::from_millis(20),
        );
        let cell = &state.badges()[0].cells[0];
        assert_eq!(cell, &BadgeCell::ReleasedModifier("⌘".to_string()));
        assert!(cell.is_modifier());
        assert!(!cell.is_accented_modifier());
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
    fn chord_badge_splits_into_one_cell_per_key() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_COMMAND, 55), DisplayMode::All, now);
        state.apply(
            down(12, keyboard::FLAG_COMMAND, "q"),
            DisplayMode::All,
            now + Duration::from_millis(10),
        );
        let badge = &state.badges()[0];
        assert_eq!(badge.kind, BadgeKind::Chord);
        assert_eq!(badge.text, "⌘q");
        assert_eq!(
            badge.cells,
            [
                BadgeCell::Modifier("⌘".to_string()),
                BadgeCell::Key("q".to_string())
            ]
        );
    }

    #[test]
    fn cell_badges_estimate_wider_than_a_single_label() {
        let cells = [
            BadgeCell::Modifier("⌘".to_string()),
            BadgeCell::Key("q".to_string()),
        ];
        assert!(estimated_cells_width(&cells, 1) > estimated_badge_width("⌘q", 1));
    }

    /// The reported bug, end to end: a bare arrow key showed as "fn ←". macOS sets
    /// `NSEventModifierFlagFunction` on the event itself (measured: `←` arrives as
    /// `flags=0x00A00100`), so the key path must not read it as a held modifier.
    #[test]
    fn a_bare_arrow_key_renders_only_the_arrow() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(123, keyboard::FLAG_FN, ""), DisplayMode::All, now);
        let badges = state.badges();
        assert_eq!(
            badges.len(),
            1,
            "one arrow key must produce exactly one badge"
        );
        assert_eq!(badges[0].text, "←");
        assert!(
            !badges[0].text.contains("fn"),
            "the system's function-key bit must not render as a held fn"
        );
        assert!(
            badges[0]
                .cells
                .iter()
                .all(|cell| !matches!(cell, BadgeCell::Modifier(_))),
            "an arrow key has no held modifier to draw"
        );
    }

    /// The same bit on a real modifier event *does* mean fn is held, and must still display.
    #[test]
    fn holding_fn_still_renders_an_fn_badge() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(flags(keyboard::FLAG_FN, 63), DisplayMode::All, now);
        let badges = state.badges();
        assert_eq!(badges.len(), 1);
        assert_eq!(badges[0].kind, BadgeKind::Modifier);
        assert_eq!(badges[0].text, "fn");
        assert_eq!(badges[0].cells, [BadgeCell::Modifier("fn".to_string())]);
    }

    #[test]
    fn the_stream_stays_capped_once_it_overflows_and_resets_when_cleared() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        for offset in 0..6u64 {
            state.apply(
                down(offset as u16, 0, "a"),
                DisplayMode::All,
                now + Duration::from_millis(offset),
            );
        }
        assert!(!state.capped());
        // A narrow cap trims the front keys and pins the stream.
        assert!(state.trim_to_extent(60.0, Orientation::Horizontal));
        assert!(state.capped());
        assert!(state.badges().len() < 6);
        state.trim_to_extent(60.0, Orientation::Horizontal);
        assert!(state.capped());
        assert!(state.tick(now + Duration::from_millis(5) + IDLE_FADE));
        assert!(!state.capped());
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
        assert_eq!(estimated_badge_width("a", 1), BADGE_MIN_WIDTH);
        assert_eq!(estimated_badge_width("abc", 1), 46.0);
        assert_eq!(estimated_badge_width("abc", 10), 76.0);
        assert_eq!(estimated_stream_width([46.0, 76.0]), 152.0);
        assert_eq!(estimated_badge_width("中文", 1), 48.0);
        assert_eq!(estimated_badge_width("🙂", 1), 34.0);
    }

    #[test]
    fn width_limit_keeps_the_current_oversized_badge_intact() {
        let now = Instant::now();
        let mut state = StateMachine::default();
        state.apply(down(0, 0, "a"), DisplayMode::All, now);
        let text = state.badges()[0].text.clone();
        assert!(!state.trim_to_extent(1.0, Orientation::Horizontal));
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
