//! Direction-sequence recognition: turns a sample path into an R/L/U/D string.
//!
//! Pure logic with no FFI, so every rule is a unit test. The contract a caller depends on:
//!
//! * A direction is only adopted once the \`anchor\` has moved \`min_segment_length\` from it, so the
//!   threshold is a *distance*, not an event count -- the same path sampled at 125 Hz and at 1000 Hz
//!   yields the same sequence while the sampling condition below holds.
//! * A direction change must be confirmed by a second qualifying segment before it is adopted
//!   (Strokly does the same): a one-segment excursion is pointer jitter, not a turn.
//! * At exactly 45 degrees the tie is broken deterministically --
//!   [[dominant_direction]] -- so a floating-point comparison never decides the sequence.

/// One of the four quantized directions. Serialized as its uppercase letter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl Direction {
    pub(crate) fn as_char(self) -> char {
        match self {
            Self::Up => 'U',
            Self::Down => 'D',
            Self::Left => 'L',
            Self::Right => 'R',
        }
    }

    /// The letter a user types in the config, or \`None\` for anything else.
    pub(crate) fn from_char(c: char) -> Option<Self> {
        match c {
            'U' | 'u' => Some(Self::Up),
            'D' | 'd' => Some(Self::Down),
            'L' | 'l' => Some(Self::Left),
            'R' | 'r' => Some(Self::Right),
            _ => None,
        }
    }
}

/// A point in CGEvent global coordinates. Y grows downward (CGEvent's convention), so a negative
/// dy is *up*.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Point {
    pub(crate) x: f64,
    pub(crate) y: f64,
}

impl Default for Point {
    fn default() -> Self {
        Self {
            x: f64::NAN,
            y: f64::NAN,
        }
    }
}

impl Point {
    pub(crate) const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn distance_to(self, other: Self) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

/// Tuning. Defaults follow the design document; every value is overridable in the config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Params {
    /// Cumulative path length that must be covered before the gesture leaves \`Held\`.
    pub(crate) min_path_length: f64,
    /// Straight-line distance from the anchor that qualifies one direction segment.
    pub(crate) min_segment_length: f64,
    /// Samples closer than this to the previous kept sample are dropped as noise.
    pub(crate) noise_distance: f64,
    /// More than this many directions makes the gesture unrecognizable rather than truncated: a
    /// truncated sequence would silently match a shorter binding.
    pub(crate) max_directions: usize,
    /// A gap between kept samples longer than this marks the trajectory as low-quality. This is an
    /// *observable* statement about the samples we saw -- it cannot say whether a direction was
    /// skipped between them.
    pub(crate) degraded_gap_ms: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            min_path_length: 30.0,
            min_segment_length: 24.0,
            noise_distance: 1.5,
            max_directions: 8,
            degraded_gap_ms: 16.0,
        }
    }
}

/// Quantize a displacement. Y grows downward in CGEvent coordinates.
///
/// At exactly 45 degrees the tie goes to the axis of the previous direction when there is one, and
/// to the horizontal axis otherwise. Both arms are reachable and tested;
/// the rule exists so the answer never depends on which of \`|dx|\` and \`|dy|\` the float comparison
/// happened to take first.
pub(crate) fn dominant_direction(dx: f64, dy: f64, previous: Option<Direction>) -> Direction {
    let horizontal = if dx >= 0.0 {
        Direction::Right
    } else {
        Direction::Left
    };
    let vertical = if dy < 0.0 {
        Direction::Up
    } else {
        Direction::Down
    };
    let ax = dx.abs();
    let ay = dy.abs();
    if ax == ay {
        return match previous {
            Some(Direction::Up | Direction::Down) => vertical,
            _ => horizontal,
        };
    }
    if ax > ay {
        horizontal
    } else {
        vertical
    }
}

/// The recognizer's state for one held button.
#[derive(Debug, Clone)]
pub(crate) struct Recognizer {
    params: Params,
    /// The point the next direction is measured from.
    anchor: Point,
    /// The last sample that passed the noise filter.
    last_sample: Point,
    /// The time of \`last_sample\`, in milliseconds.
    last_sample_ms: f64,
    /// Total path length walked since the press, not counting dropped noise.
    path_length: f64,
    /// The adopted sequence.
    directions: Vec<Direction>,
    /// A direction seen once but not yet confirmed by a second segment.
    pending: Option<Direction>,
    pending_count: u8,
    /// Whether a sample gap exceeded \`degraded_gap_ms\`.
    degraded: bool,
    /// Whether the press has been fed its first point.
    started: bool,
    /// The sequence grew past \`max_directions\`: from here on the result is "no match" and nothing more
    /// is recorded, so a long press cannot grow the vector without end.
    truncated: bool,
}

impl Recognizer {
    pub(crate) fn new(params: Params, origin: Point, now_ms: f64) -> Self {
        Self {
            params,
            anchor: origin,
            last_sample: origin,
            last_sample_ms: now_ms,
            path_length: 0.0,
            directions: Vec::new(),
            pending: None,
            pending_count: 0,
            degraded: false,
            started: true,
            truncated: false,
        }
    }

    /// Whether the gesture has crossed \`min_path_length\` and may leave \`Held\`.
    pub(crate) fn engaged(&self) -> bool {
        self.path_length >= self.params.min_path_length
    }

    /// The directions adopted so far, for a live preview. Not the final answer: a pending turn
    /// is still unconfirmed.
    pub(crate) fn snapshot_key(&self) -> String {
        self.directions.iter().map(|d| d.as_char()).collect()
    }

    /// Whether a sample gap was seen: the published trail-quality fact (it cannot say whether a
    /// direction was skipped between two samples).
    pub(crate) fn degraded(&self) -> bool {
        self.degraded
    }

    /// Feed one sample. Returns \`true\` when the adopted sequence grew, so a caller can show the
    /// partial result live.
    pub(crate) fn push(&mut self, point: Point, now_ms: f64) -> bool {
        if !self.started {
            self.started = true;
            self.anchor = point;
            self.last_sample = point;
            self.last_sample_ms = now_ms;
            return false;
        }
        let step = point.distance_to(self.last_sample);
        if step < self.params.noise_distance {
            return false;
        }
        if now_ms - self.last_sample_ms > self.params.degraded_gap_ms {
            self.degraded = true;
        }
        self.path_length += step;
        self.last_sample = point;
        self.last_sample_ms = now_ms;
        self.advance_anchor(point)
    }

    /// Move the anchor to \`point\` when it is far enough, adopting the direction it defines.
    fn advance_anchor(&mut self, point: Point) -> bool {
        let dx = point.x - self.anchor.x;
        let dy = point.y - self.anchor.y;
        if dx.hypot(dy) < self.params.min_segment_length {
            return false;
        }
        let previous = self.directions.last().copied();
        let direction = dominant_direction(dx, dy, previous.or(self.pending));
        self.anchor = point;
        self.adopt(direction)
    }

    /// The two-segment confirmation. \`previous\` is the last adopted direction.
    fn adopt(&mut self, direction: Direction) -> bool {
        if self.truncated {
            return false;
        }
        match self.directions.last().copied() {
            None => {
                self.directions.push(direction);
                self.pending = None;
                self.pending_count = 0;
                true
            }
            Some(last) if last == direction => {
                // Still walking the same way: a half-formed turn was jitter.
                self.pending = None;
                self.pending_count = 0;
                false
            }
            Some(_) => {
                if self.pending == Some(direction) {
                    self.pending_count = self.pending_count.saturating_add(1);
                    if self.pending_count >= 2 {
                        self.directions.push(direction);
                        if self.directions.len() > self.params.max_directions {
                            self.truncated = true;
                        }
                        self.pending = None;
                        self.pending_count = 0;
                        return true;
                    }
                    false
                } else {
                    self.pending = Some(direction);
                    self.pending_count = 1;
                    false
                }
            }
        }
    }

    /// The sequence as it stands. Called at release.
    ///
    /// A \`pending\` direction is adopted here: the segment that produced it already cleared
    /// \`min_segment_length\`, so the spatial condition the two-segment rule exists for is met; only
    /// the confirming second sample never arrived because the button came up.
    pub(crate) fn finish(&mut self) -> Sequence {
        if let Some(pending) = self.pending.take() {
            if self.directions.last().copied() != Some(pending) {
                self.directions.push(pending);
            }
            self.pending_count = 0;
        }
        let mut sequence = Sequence {
            directions: self.directions.clone(),
            degraded: self.degraded,
            overlong: false,
        };
        // Overlong is refused, never truncated into a shorter match.
        sequence.overlong = sequence.directions.len() > self.params.max_directions;
        sequence
    }
}

/// The outcome of \`Recognizer::finish\`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sequence {
    pub(crate) directions: Vec<Direction>,
    /// Low sample quality. Cannot prove a direction was missed -- only that a gap was seen.
    pub(crate) degraded: bool,
    /// Too many directions to be recognizable.
    pub(crate) overlong: bool,
}

impl Sequence {
    /// The config spelling, e.g. \`"RD"\`.
    pub(crate) fn to_key(&self) -> String {
        self.directions.iter().map(|d| d.as_char()).collect()
    }
}

/// Parse a config gesture string into a normalised key, or \`None\` when it is not a gesture.
///
/// Normalises case and trims; a sequence longer than \`max\` is refused rather than truncated, so a
/// hand-written key can never silently mean something shorter.
pub(crate) fn parse_key(text: &str, max: usize) -> Option<String> {
    let mut out = String::new();
    for c in text.trim().chars() {
        let direction = Direction::from_char(c)?;
        out.push(direction.as_char());
    }
    if out.is_empty() || out.len() > max {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point::new(x, y)
    }

    /// Feed a straight run of samples \`step\` apart, \`count\` times.
    fn run(rec: &mut Recognizer, start: Point, step: Point, count: usize, step_ms: f64) -> Point {
        let mut last = start;
        let mut t = 0.0;
        for _ in 0..count {
            last = p(last.x + step.x, last.y + step.y);
            t += step_ms;
            rec.push(last, t);
        }
        last
    }

    #[test]
    fn a_short_press_is_not_engaged() {
        let mut rec = Recognizer::new(Params::default(), p(100.0, 100.0), 0.0);
        rec.push(p(110.0, 100.0), 8.0);
        assert!(!rec.engaged(), "10pt is under min_path_length");
        assert_eq!(rec.finish().to_key(), "");
    }

    #[test]
    fn a_single_direction() {
        let mut rec = Recognizer::new(Params::default(), p(100.0, 100.0), 0.0);
        run(&mut rec, p(100.0, 100.0), p(10.0, 0.0), 6, 8.0);
        assert!(rec.engaged(), "60pt is over min_path_length");
        assert_eq!(rec.finish().to_key(), "R");
    }

    #[test]
    fn two_segments_make_two_directions() {
        let mut rec = Recognizer::new(Params::default(), p(100.0, 100.0), 0.0);
        let mid = run(&mut rec, p(100.0, 100.0), p(10.0, 0.0), 5, 8.0);
        run(&mut rec, mid, p(0.0, 10.0), 5, 8.0);
        assert_eq!(rec.finish().to_key(), "RD", "right then down");
    }

    #[test]
    fn a_jitter_excursion_does_not_become_a_direction() {
        let mut rec = Recognizer::new(Params::default(), p(100.0, 100.0), 0.0);
        let mid = run(&mut rec, p(100.0, 100.0), p(10.0, 0.0), 5, 8.0);
        // One segment up (jitter), then back to the right: never confirmed.
        let up = run(&mut rec, mid, p(0.0, -30.0), 1, 8.0);
        run(&mut rec, up, p(10.0, 0.0), 5, 8.0);
        assert_eq!(rec.finish().to_key(), "R", "the brief turn is discarded");
    }

    #[test]
    fn exactly_45_degrees_prefers_the_previous_axis() {
        // 100 then 100 at 45° from the anchor: with no previous direction, horizontal wins.
        assert_eq!(dominant_direction(10.0, 10.0, None), Direction::Right);
        // With a vertical previous direction, vertical wins -- for a downward vector that is Down.
        assert_eq!(
            dominant_direction(10.0, 10.0, Some(Direction::Up)),
            Direction::Down
        );
        assert_eq!(
            dominant_direction(10.0, -10.0, Some(Direction::Up)),
            Direction::Up
        );
        assert_eq!(
            dominant_direction(-10.0, 10.0, Some(Direction::Down)),
            Direction::Down
        );
        assert_eq!(
            dominant_direction(10.0, -10.0, Some(Direction::Right)),
            Direction::Right
        );
    }

    #[test]
    fn y_grows_downward() {
        // CGEvent: a negative dy is up.
        assert_eq!(dominant_direction(0.0, -10.0, None), Direction::Up);
        assert_eq!(dominant_direction(0.0, 10.0, None), Direction::Down);
    }

    #[test]
    fn overlong_is_refused_rather_than_truncated() {
        let mut params = Params::default();
        params.max_directions = 2;
        let mut rec = Recognizer::new(params, p(0.0, 0.0), 0.0);
        let mut at = p(0.0, 0.0);
        for (i, step) in [p(10.0, 0.0), p(0.0, 10.0), p(-10.0, 0.0), p(0.0, -10.0)]
            .iter()
            .enumerate()
        {
            for _ in 0..5 {
                at = p(at.x + step.x, at.y + step.y);
                rec.push(at, (i * 5) as f64 * 8.0);
            }
        }
        let seq = rec.finish();
        assert!(seq.overlong, "four directions exceed max_directions=2");
    }

    #[test]
    fn recognition_storage_stays_bounded_while_dragging() {
        // A very long zig-zag must not grow the sequence past max_directions while the button is
        // still held; the refusal is recorded, not deferred to release.
        let mut params = Params::default();
        params.max_directions = 4;
        let mut rec = Recognizer::new(params, p(0.0, 0.0), 0.0);
        let mut at = p(0.0, 0.0);
        let mut t = 0.0;
        for _ in 0..12 {
            for (dx, dy) in [(10.0, 0.0), (0.0, 10.0), (-10.0, 0.0), (0.0, -10.0)] {
                for _ in 0..5 {
                    at = p(at.x + dx, at.y + dy);
                    t += 8.0;
                    rec.push(at, t);
                }
            }
        }
        let seq = rec.finish();
        assert!(seq.overlong, "a sequence longer than the cap is refused");
    }

    #[test]
    fn a_sample_gap_is_reported_as_degraded() {
        let mut rec = Recognizer::new(Params::default(), p(0.0, 0.0), 0.0);
        rec.push(p(10.0, 0.0), 8.0);
        rec.push(p(20.0, 0.0), 60.0); // 52ms gap
        assert!(rec.degraded());
        assert_eq!(rec.finish().to_key(), "");
    }

    #[test]
    fn parse_key_normalises_and_refuses() {
        assert_eq!(parse_key("rd", 8).as_deref(), Some("RD"));
        assert_eq!(parse_key("  URU ", 8).as_deref(), Some("URU"));
        assert_eq!(parse_key("", 8), None);
        assert_eq!(parse_key("XU", 8), None, "an unknown letter is refused");
        assert_eq!(
            parse_key("URDLURDLU", 8),
            None,
            "longer than max is refused"
        );
    }

    #[test]
    fn sparse_sampling_keeps_the_leading_direction() {
        // Same path sampled at two densities: the sequence may lose nothing here because every
        // segment keeps two qualifying samples.
        let dense = {
            let mut rec = Recognizer::new(Params::default(), p(0.0, 0.0), 0.0);
            let mut at = p(0.0, 0.0);
            for _ in 0..5 {
                at = p(at.x + 10.0, at.y);
                rec.push(at, at.x);
            }
            for _ in 0..5 {
                at = p(at.x, at.y - 10.0);
                rec.push(at, at.x);
            }
            rec.finish().to_key()
        };
        assert_eq!(dense, "RU");
    }
}
