// MIT License
// Copyright (c) 2021-2024 LinearMouse

//! Pure CG scroll-phase and subpixel delivery math ported from LinearMouse.

use super::engine::Phase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PhaseFields {
    pub scroll_phase: i64,
    pub momentum_phase: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DeliveredDelta {
    pub fixed_x: f64,
    pub fixed_y: f64,
    pub point_x: i64,
    pub point_y: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct SubpixelAccumulator {
    x: f64,
    y: f64,
}

pub(crate) const POINTS_PER_INPUT_LINE: f64 = 36.0;
pub(crate) const POINTS_PER_INTEGER_SCROLL_UNIT: f64 = 12.0;

pub(crate) fn phase_fields(phase: Phase) -> PhaseFields {
    match phase {
        Phase::TouchBegan => PhaseFields {
            scroll_phase: 1,
            momentum_phase: 0,
        },
        Phase::TouchChanged => PhaseFields {
            scroll_phase: 2,
            momentum_phase: 0,
        },
        Phase::TouchEnded => PhaseFields {
            scroll_phase: 4,
            momentum_phase: 0,
        },
        Phase::MomentumBegan => PhaseFields {
            scroll_phase: 0,
            momentum_phase: 1,
        },
        Phase::MomentumChanged => PhaseFields {
            scroll_phase: 0,
            momentum_phase: 2,
        },
        Phase::MomentumEnded => PhaseFields {
            scroll_phase: 0,
            momentum_phase: 3,
        },
    }
}

impl SubpixelAccumulator {
    pub(crate) fn convert(&mut self, delta_x: f64, delta_y: f64, phase: Phase) -> DeliveredDelta {
        let (point_x, point_y) = match phase {
            Phase::MomentumBegan | Phase::MomentumChanged | Phase::MomentumEnded => {
                self.x = 0.0;
                self.y = 0.0;
                (delta_x.trunc() as i64, delta_y.trunc() as i64)
            }
            Phase::TouchBegan | Phase::TouchChanged | Phase::TouchEnded => {
                self.x += delta_x;
                self.y += delta_y;
                let point_x = self.x.trunc() as i64;
                let point_y = self.y.trunc() as i64;
                self.x -= point_x as f64;
                self.y -= point_y as f64;
                (point_x, point_y)
            }
        };
        DeliveredDelta {
            fixed_x: delta_x,
            fixed_y: delta_y,
            point_x,
            point_y,
        }
    }

    #[cfg(test)]
    pub(crate) fn remainder(&self) -> (f64, f64) {
        (self.x, self.y)
    }
}

/// LinearMouse's legacy integer wheel fields express 12 points per unit. Rust's `trunc` matches
/// Swift's `Int(Double)` rounding toward zero for positive and negative deltas.
pub(crate) fn integer_delta(delta_points: f64) -> i64 {
    (delta_points / POINTS_PER_INTEGER_SCROLL_UNIT).trunc() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_fields_follow_scroll_and_momentum_sequences() {
        let phases = [
            (Phase::TouchBegan, 1, 0),
            (Phase::TouchChanged, 2, 0),
            (Phase::TouchEnded, 4, 0),
            (Phase::MomentumBegan, 0, 1),
            (Phase::MomentumChanged, 0, 2),
            (Phase::MomentumEnded, 0, 3),
        ];
        for (phase, scroll, momentum) in phases {
            assert_eq!(
                phase_fields(phase),
                PhaseFields {
                    scroll_phase: scroll,
                    momentum_phase: momentum
                }
            );
        }
    }

    #[test]
    fn subpixel_remainders_accumulate_then_momentum_truncates_current_delta() {
        let mut accumulator = SubpixelAccumulator::default();
        assert_eq!(accumulator.convert(0.4, -0.4, Phase::TouchBegan).point_x, 0);
        let second = accumulator.convert(0.7, -0.7, Phase::TouchChanged);
        assert_eq!((second.point_x, second.point_y), (1, -1));
        assert_eq!(
            accumulator.remainder(),
            (0.10000000000000009, -0.10000000000000009)
        );
        let momentum = accumulator.convert(0.8, -1.8, Phase::MomentumBegan);
        assert_eq!((momentum.point_x, momentum.point_y), (0, -1));
        assert_eq!((momentum.fixed_x, momentum.fixed_y), (0.8, -1.8));
        assert_eq!(accumulator.remainder(), (0.0, 0.0));
    }

    #[test]
    fn legacy_integer_units_truncate_toward_zero() {
        assert_eq!(integer_delta(35.9), 2);
        assert_eq!(integer_delta(-35.9), -2);
    }
}
