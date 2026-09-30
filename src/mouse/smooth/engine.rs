// MIT License
// Copyright (c) 2021-2024 LinearMouse

//! Pure, virtual-time port of LinearMouse's smoothed wheel engine.

use super::presets::{SmoothPreset, SmoothSettings};

const INPUT_GRACE: f64 = 1.0 / 25.0;
const STOP_THRESHOLD: f64 = 0.5;
const AXIS_ACTIVITY_THRESHOLD: f64 = 0.01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    TouchBegan,
    TouchChanged,
    TouchEnded,
    MomentumBegan,
    MomentumChanged,
    MomentumEnded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputKind {
    Wheel,
    #[allow(dead_code)]
    ContinuousGesture,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Emission {
    pub delta_x: f64,
    pub delta_y: f64,
    pub phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionState {
    Idle,
    Touching,
    Momentum,
}

#[derive(Debug, Clone, Copy)]
struct AxisTuning {
    preset: SmoothPreset,
    response: f64,
    speed: f64,
    acceleration: f64,
    inertia: f64,
}

impl AxisTuning {
    const LEGACY_UPPER_BOUND: f64 = 3.0;

    fn new(settings: SmoothSettings) -> Self {
        Self {
            preset: settings.preset,
            response: settings.response.clamp(0.0, 2.0),
            speed: settings.speed.clamp(0.0, 8.0),
            acceleration: settings.acceleration.clamp(0.0, 8.0),
            inertia: settings.inertia.clamp(0.0, 8.0),
        }
    }

    fn desired_velocity(self, input: f64) -> f64 {
        if input == 0.0 {
            return 0.0;
        }
        let profile = self.preset.profile();
        let base_magnitude = input.abs();
        let normalized_magnitude = (base_magnitude / (base_magnitude + 24.0)).clamp(0.0, 1.0);
        let curved_magnitude = normalized_magnitude.powf(profile.input_exponent);
        let magnitude = base_magnitude * curved_magnitude;
        let speed_boost = 0.85 + self.speed * 0.4;
        let acceleration_boost = 1.0 + self.acceleration * profile.acceleration_gain;
        input.signum() * magnitude * profile.velocity_scale * speed_boost * acceleration_boost
    }

    fn reengagement_dominance(self, input_velocity: f64, current_velocity: f64) -> f64 {
        if input_velocity == 0.0 || current_velocity == 0.0 {
            0.0
        } else {
            (current_velocity.abs() / input_velocity.abs()).clamp(0.0, 1.0)
        }
    }

    fn tail_recovery(self, input_velocity: f64, current_velocity: f64) -> f64 {
        ((0.75 - self.reengagement_dominance(input_velocity, current_velocity)) / 0.75)
            .clamp(0.0, 1.0)
    }

    fn reengaged_desired_velocity(self, input: f64, current_velocity: f64) -> f64 {
        let input_velocity = self.desired_velocity(input);
        if current_velocity == 0.0 {
            return input_velocity;
        }
        if input_velocity.signum() == current_velocity.signum() {
            let profile = self.preset.profile();
            let carry_factor =
                (0.06 + self.response * 0.06 + self.acceleration * 0.01).clamp(0.06, 0.16);
            let ceiling_factor =
                (1.01 + profile.response * 0.08 + self.response * 0.06).clamp(1.02, 1.12);
            let carried_magnitude = (current_velocity.abs() + input_velocity.abs() * carry_factor)
                .min(current_velocity.abs().max(input_velocity.abs()) * ceiling_factor);
            let recovery = self
                .tail_recovery(input_velocity, current_velocity)
                .powf(0.8);
            let target_magnitude =
                carried_magnitude + (input_velocity.abs() - carried_magnitude) * recovery;
            current_velocity.signum() * target_magnitude
        } else {
            let braking_blend = (0.50 + self.response * 0.20).clamp(0.50, 0.82);
            current_velocity + (input_velocity - current_velocity) * braking_blend
        }
    }

    fn residual_input_after_cancelling_momentum(self, input: f64, current_velocity: f64) -> f64 {
        let input_velocity = self.desired_velocity(input);
        let input_magnitude = input_velocity.abs();
        let current_magnitude = current_velocity.abs();
        if input_magnitude <= current_magnitude {
            0.0
        } else {
            let residual_fraction =
                ((input_magnitude - current_magnitude) / input_magnitude).clamp(0.0, 1.0);
            input * residual_fraction
        }
    }

    fn blend_factor(self, dt: f64) -> f64 {
        let scaled = self.preset.profile().response * 0.75 + self.response * 0.8;
        (scaled * dt * 60.0).clamp(0.0, 1.0)
    }

    fn reengagement_blend_factor(
        self,
        dt: f64,
        desired_velocity: f64,
        current_velocity: f64,
    ) -> f64 {
        let base_blend = self.blend_factor(dt);
        let softened_blend = (base_blend * (0.10 + self.response * 0.08)).clamp(0.0, 0.12);
        let recovery = self
            .tail_recovery(desired_velocity, current_velocity)
            .powf(0.55);
        (softened_blend + (base_blend - softened_blend) * recovery)
            .clamp(softened_blend, base_blend)
    }

    fn reengagement_kick_factor(self, desired_velocity: f64, current_velocity: f64) -> f64 {
        if desired_velocity == 0.0
            || current_velocity == 0.0
            || desired_velocity.signum() != current_velocity.signum()
        {
            return 0.0;
        }
        let recovery = self
            .tail_recovery(desired_velocity, current_velocity)
            .powf(0.8);
        let base_kick = (0.04 + self.response * 0.04).clamp(0.04, 0.08);
        let tail_kick = (0.14 + self.response * 0.05 + self.acceleration * 0.02).clamp(0.14, 0.28);
        (base_kick + tail_kick * recovery).clamp(0.04, 0.24)
    }

    fn momentum_decay(self, dt: f64) -> f64 {
        let profile = self.preset.profile();
        let legacy_inertia_boost =
            ((self.inertia.min(Self::LEGACY_UPPER_BOUND) - 0.65) * 0.05).clamp(-0.08, 0.10);
        let extended_inertia_boost = (self.inertia - Self::LEGACY_UPPER_BOUND).max(0.0) * 0.01;
        let decay_ceiling = if self.inertia > Self::LEGACY_UPPER_BOUND {
            0.99
        } else {
            0.98
        };
        let dt_scale = (dt * 60.0).max(0.25);
        (profile.decay + legacy_inertia_boost + extended_inertia_boost)
            .clamp(0.72, decay_ceiling)
            .powf(dt_scale)
    }
}

#[derive(Debug, Clone, Copy)]
enum AxisBehavior {
    #[cfg(test)]
    Passthrough,
    Smoothed(AxisTuning),
}

#[derive(Debug, Clone)]
pub(crate) struct SmoothEngine {
    horizontal_behavior: AxisBehavior,
    vertical_behavior: AxisBehavior,
    session_state: SessionState,
    last_tick_timestamp: Option<f64>,
    last_input_timestamp: Option<f64>,
    pending_input_x: f64,
    pending_input_y: f64,
    estimator_x: WheelInputVelocityEstimator,
    estimator_y: WheelInputVelocityEstimator,
    desired_velocity_x: f64,
    desired_velocity_y: f64,
    velocity_x: f64,
    velocity_y: f64,
    touch_has_begun: bool,
    pending_momentum_begin: bool,
    reengaged_from_momentum: bool,
}

impl SmoothEngine {
    pub(crate) fn new(settings: SmoothSettings) -> Self {
        let behavior = AxisBehavior::Smoothed(AxisTuning::new(settings));
        Self {
            horizontal_behavior: behavior,
            vertical_behavior: behavior,
            session_state: SessionState::Idle,
            last_tick_timestamp: None,
            last_input_timestamp: None,
            pending_input_x: 0.0,
            pending_input_y: 0.0,
            estimator_x: WheelInputVelocityEstimator::default(),
            estimator_y: WheelInputVelocityEstimator::default(),
            desired_velocity_x: 0.0,
            desired_velocity_y: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            touch_has_begun: false,
            pending_momentum_begin: false,
            reengaged_from_momentum: false,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        match self.session_state {
            SessionState::Idle => self.pending_input_x != 0.0 || self.pending_input_y != 0.0,
            SessionState::Touching | SessionState::Momentum => true,
        }
    }

    pub(crate) fn exclusive_active_axis(&self) -> Option<Axis> {
        let horizontal = axis_is_active(
            self.pending_input_x,
            self.desired_velocity_x,
            self.velocity_x,
        );
        let vertical = axis_is_active(
            self.pending_input_y,
            self.desired_velocity_y,
            self.velocity_y,
        );
        match (horizontal, vertical) {
            (true, false) => Some(Axis::Horizontal),
            (false, true) => Some(Axis::Vertical),
            _ => None,
        }
    }

    pub(crate) fn reset_other_axis(&mut self, incoming_axis: Axis) {
        let Some(active_axis) = self.exclusive_active_axis() else {
            return;
        };
        if active_axis == incoming_axis {
            return;
        }
        match active_axis {
            Axis::Horizontal => {
                self.pending_input_x = 0.0;
                self.estimator_x.reset();
                self.desired_velocity_x = 0.0;
                self.velocity_x = 0.0;
            }
            Axis::Vertical => {
                self.pending_input_y = 0.0;
                self.estimator_y.reset();
                self.desired_velocity_y = 0.0;
                self.velocity_y = 0.0;
            }
        }
        if self.velocity_x.abs() <= STOP_THRESHOLD
            && self.velocity_y.abs() <= STOP_THRESHOLD
            && self.pending_input_x == 0.0
            && self.pending_input_y == 0.0
        {
            self.pending_momentum_begin = false;
            self.reengaged_from_momentum = false;
            if self.session_state == SessionState::Momentum {
                self.session_state = SessionState::Idle;
                self.touch_has_begun = false;
            }
        }
    }

    pub(crate) fn feed(
        &mut self,
        delta_x: f64,
        delta_y: f64,
        timestamp: f64,
        input_kind: InputKind,
    ) {
        let (mut delta_x, mut delta_y) = (delta_x, delta_y);
        if self.session_state == SessionState::Momentum {
            self.cancel_opposing_momentum(&mut delta_x, &mut delta_y);
        }
        self.pending_input_x += delta_x;
        self.pending_input_y += delta_y;
        self.update_velocity_estimators(delta_x, delta_y, timestamp, input_kind);
        self.last_input_timestamp = Some(timestamp);

        if self.session_state == SessionState::Idle && (delta_x != 0.0 || delta_y != 0.0) {
            self.session_state = SessionState::Touching;
            self.touch_has_begun = false;
            self.pending_momentum_begin = false;
            self.last_tick_timestamp = Some(timestamp);
        } else if self.session_state == SessionState::Momentum && (delta_x != 0.0 || delta_y != 0.0)
        {
            self.session_state = SessionState::Touching;
            self.touch_has_begun = false;
            self.pending_momentum_begin = false;
            self.reengaged_from_momentum = true;
        }
        if self.last_tick_timestamp.is_none() {
            self.last_tick_timestamp = Some(timestamp);
        }
    }

    fn cancel_opposing_momentum(&mut self, delta_x: &mut f64, delta_y: &mut f64) {
        cancel_opposing(
            *delta_x,
            self.horizontal_behavior,
            &mut self.desired_velocity_x,
            &mut self.velocity_x,
            delta_x,
        );
        cancel_opposing(
            *delta_y,
            self.vertical_behavior,
            &mut self.desired_velocity_y,
            &mut self.velocity_y,
            delta_y,
        );
    }

    fn update_velocity_estimators(&mut self, dx: f64, dy: f64, timestamp: f64, kind: InputKind) {
        match kind {
            InputKind::Wheel => {
                self.estimator_x.add(dx, timestamp);
                self.estimator_y.add(dy, timestamp);
            }
            InputKind::ContinuousGesture => {
                if dx != 0.0 {
                    self.estimator_x.reset();
                }
                if dy != 0.0 {
                    self.estimator_y.reset();
                }
            }
        }
    }

    pub(crate) fn advance(&mut self, timestamp: f64) -> Option<Emission> {
        let previous_tick = self.last_tick_timestamp.unwrap_or(timestamp);
        let dt = (timestamp - previous_tick).clamp(1.0 / 240.0, 1.0 / 24.0);
        self.last_tick_timestamp = Some(timestamp);
        let has_pending_input = self.pending_input_x != 0.0 || self.pending_input_y != 0.0;
        let has_fresh_input = self
            .last_input_timestamp
            .is_some_and(|last| timestamp - last <= INPUT_GRACE);
        let blend_reengagement = self.reengaged_from_momentum && has_pending_input;
        let effective_x = self
            .estimator_x
            .projected_input(self.pending_input_x, timestamp);
        let effective_y = self
            .estimator_y
            .projected_input(self.pending_input_y, timestamp);
        let tick = AxisTick {
            has_pending_input,
            has_fresh_input,
            reengaged_from_momentum: blend_reengagement,
            dt,
        };
        let emission_x = advance_axis(
            self.horizontal_behavior,
            &mut self.pending_input_x,
            effective_x,
            &mut self.desired_velocity_x,
            &mut self.velocity_x,
            tick,
        );
        let emission_y = advance_axis(
            self.vertical_behavior,
            &mut self.pending_input_y,
            effective_y,
            &mut self.desired_velocity_y,
            &mut self.velocity_y,
            tick,
        );
        self.reengaged_from_momentum = false;
        let has_movement = emission_x.abs() >= 0.01 || emission_y.abs() >= 0.01;
        let should_continue_momentum =
            self.velocity_x.abs() > STOP_THRESHOLD || self.velocity_y.abs() > STOP_THRESHOLD;

        match self.session_state {
            SessionState::Idle => None,
            SessionState::Touching => {
                if has_fresh_input {
                    if !has_movement {
                        return None;
                    }
                    let phase = if self.touch_has_begun {
                        Phase::TouchChanged
                    } else {
                        Phase::TouchBegan
                    };
                    self.touch_has_begun = true;
                    Some(Emission {
                        delta_x: emission_x,
                        delta_y: emission_y,
                        phase,
                    })
                } else if should_continue_momentum {
                    self.session_state = SessionState::Momentum;
                    self.pending_momentum_begin = true;
                    self.touch_has_begun = false;
                    self.estimator_x.reset();
                    self.estimator_y.reset();
                    Some(Emission {
                        delta_x: 0.0,
                        delta_y: 0.0,
                        phase: Phase::TouchEnded,
                    })
                } else {
                    self.session_state = SessionState::Idle;
                    self.estimator_x.reset();
                    self.estimator_y.reset();
                    self.velocity_x = 0.0;
                    self.velocity_y = 0.0;
                    self.desired_velocity_x = 0.0;
                    self.desired_velocity_y = 0.0;
                    self.touch_has_begun = false;
                    Some(Emission {
                        delta_x: emission_x,
                        delta_y: emission_y,
                        phase: Phase::TouchEnded,
                    })
                }
            }
            SessionState::Momentum => {
                if !has_movement && !should_continue_momentum {
                    self.session_state = SessionState::Idle;
                    self.estimator_x.reset();
                    self.estimator_y.reset();
                    self.velocity_x = 0.0;
                    self.velocity_y = 0.0;
                    self.desired_velocity_x = 0.0;
                    self.desired_velocity_y = 0.0;
                    self.touch_has_begun = false;
                    self.pending_momentum_begin = false;
                    return Some(Emission {
                        delta_x: 0.0,
                        delta_y: 0.0,
                        phase: Phase::MomentumEnded,
                    });
                }
                let phase = if self.pending_momentum_begin {
                    self.pending_momentum_begin = false;
                    Phase::MomentumBegan
                } else {
                    Phase::MomentumChanged
                };
                Some(Emission {
                    delta_x: emission_x,
                    delta_y: emission_y,
                    phase,
                })
            }
        }
    }
}

fn cancel_opposing(
    delta: f64,
    behavior: AxisBehavior,
    desired_velocity: &mut f64,
    velocity: &mut f64,
    output_delta: &mut f64,
) {
    if delta == 0.0 || *velocity == 0.0 || delta.signum() == velocity.signum() {
        return;
    }
    *output_delta = match behavior {
        #[cfg(test)]
        AxisBehavior::Passthrough => delta,
        AxisBehavior::Smoothed(tuning) => {
            tuning.residual_input_after_cancelling_momentum(delta, *velocity)
        }
    };
    *desired_velocity = 0.0;
    *velocity = 0.0;
}

#[derive(Clone, Copy)]
struct AxisTick {
    has_pending_input: bool,
    has_fresh_input: bool,
    reengaged_from_momentum: bool,
    dt: f64,
}

fn advance_axis(
    behavior: AxisBehavior,
    pending_input: &mut f64,
    effective_input: f64,
    desired_velocity: &mut f64,
    velocity: &mut f64,
    tick: AxisTick,
) -> f64 {
    match behavior {
        #[cfg(test)]
        AxisBehavior::Passthrough => {
            let output = *pending_input;
            *pending_input = 0.0;
            output
        }
        AxisBehavior::Smoothed(tuning) => {
            if *pending_input != 0.0 {
                *desired_velocity = if tick.reengaged_from_momentum {
                    tuning.reengaged_desired_velocity(effective_input, *velocity)
                } else {
                    tuning.desired_velocity(effective_input)
                };
                if tick.reengaged_from_momentum {
                    let kick = tuning.reengagement_kick_factor(*desired_velocity, *velocity);
                    *velocity += (*desired_velocity - *velocity) * kick;
                }
                *pending_input = 0.0;
            }
            if tick.has_fresh_input || tick.has_pending_input {
                let blend = if tick.reengaged_from_momentum {
                    tuning.reengagement_blend_factor(tick.dt, *desired_velocity, *velocity)
                } else {
                    tuning.blend_factor(tick.dt)
                };
                *velocity += (*desired_velocity - *velocity) * blend;
            } else {
                *velocity *= tuning.momentum_decay(tick.dt);
            }
            *velocity * tick.dt
        }
    }
}

fn axis_is_active(pending_input: f64, desired_velocity: f64, velocity: f64) -> bool {
    pending_input.abs() >= AXIS_ACTIVITY_THRESHOLD
        || desired_velocity.abs() >= AXIS_ACTIVITY_THRESHOLD
        || velocity.abs() >= AXIS_ACTIVITY_THRESHOLD
}

#[derive(Debug, Clone, Copy, Default)]
struct InputSample {
    delta: f64,
    timestamp: f64,
}

#[derive(Debug, Clone, Default)]
struct WheelInputVelocityEstimator {
    rate_adjusted_input: f64,
    direction: i8,
    last_timestamp: Option<f64>,
    recent_inputs: Vec<InputSample>,
}

impl WheelInputVelocityEstimator {
    const INPUT_GAP_RESET_INTERVAL: f64 = 1.0 / 25.0;
    const RATE_SMOOTHING_TIME_CONSTANT: f64 = 1.0 / 20.0;
    const PROJECTED_GESTURE_INTERVAL: f64 = 1.0 / 15.0;
    const RECENT_INPUT_LIMIT_INTERVAL: f64 = Self::INPUT_GAP_RESET_INTERVAL * 4.0;

    fn add(&mut self, delta: f64, timestamp: f64) {
        if delta == 0.0 {
            return;
        }
        self.advance(timestamp);
        let current_direction = if delta > 0.0 { 1 } else { -1 };
        if self.direction != 0 && current_direction != self.direction {
            self.rate_adjusted_input = 0.0;
            self.recent_inputs.clear();
        }
        self.direction = current_direction;
        self.rate_adjusted_input +=
            delta * Self::PROJECTED_GESTURE_INTERVAL / Self::RATE_SMOOTHING_TIME_CONSTANT;
        self.recent_inputs.push(InputSample { delta, timestamp });
    }

    fn projected_input(&mut self, pending_input: f64, timestamp: f64) -> f64 {
        if pending_input == 0.0 {
            self.advance(timestamp);
            return 0.0;
        }
        self.advance(timestamp);
        let mut projected = pending_input;
        if self.rate_adjusted_input != 0.0
            && self.rate_adjusted_input.signum() == pending_input.signum()
            && self.rate_adjusted_input.abs() > projected.abs()
        {
            projected = self.rate_adjusted_input;
        }
        let recent_input: f64 = self.recent_inputs.iter().map(|sample| sample.delta).sum();
        if recent_input == 0.0 || recent_input.signum() != projected.signum() {
            return projected;
        }
        if projected.abs() > recent_input.abs() {
            let capped_magnitude = pending_input.abs().max(recent_input.abs());
            projected = projected.signum() * capped_magnitude;
        }
        projected
    }

    fn reset(&mut self) {
        self.rate_adjusted_input = 0.0;
        self.direction = 0;
        self.last_timestamp = None;
        self.recent_inputs.clear();
    }

    fn advance(&mut self, timestamp: f64) {
        let Some(last_timestamp) = self.last_timestamp else {
            self.last_timestamp = Some(timestamp);
            return;
        };
        let dt = (timestamp - last_timestamp).max(0.0);
        self.last_timestamp = Some(timestamp);
        if dt > Self::INPUT_GAP_RESET_INTERVAL {
            self.rate_adjusted_input = 0.0;
            self.direction = 0;
            self.recent_inputs.clear();
            return;
        }
        self.rate_adjusted_input *= (-dt / Self::RATE_SMOOTHING_TIME_CONSTANT).exp();
        if self.rate_adjusted_input.abs() < 0.001 {
            self.rate_adjusted_input = 0.0;
        }
        self.recent_inputs
            .retain(|sample| timestamp - sample.timestamp <= Self::RECENT_INPUT_LIMIT_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(preset: SmoothPreset) -> SmoothSettings {
        let (response, speed, acceleration, inertia) = preset.default_settings();
        SmoothSettings {
            preset,
            response,
            speed,
            acceleration,
            inertia,
        }
    }

    fn collect(engine: &mut SmoothEngine, start: f64, end: f64, step: f64) -> Vec<Emission> {
        let mut output = Vec::new();
        let mut time = start;
        while time <= end + 1e-9 {
            if let Some(emission) = engine.advance(time) {
                output.push(emission);
            }
            time += step;
        }
        output
    }

    #[derive(Clone, Copy)]
    struct TimedInput {
        timestamp: f64,
        delta_y: f64,
    }

    fn timed_emissions(inputs: &[TimedInput]) -> Vec<(f64, Emission)> {
        let mut engine = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        let mut inputs = inputs.to_vec();
        inputs.sort_by(|left, right| left.timestamp.total_cmp(&right.timestamp));
        let final_time = inputs.last().map_or(0.0, |input| input.timestamp) + 0.5;
        let tick_count = (final_time * 120.0).ceil() as usize;
        let mut next_input = 0;
        let mut output = Vec::new();
        for tick in 1..=tick_count {
            let timestamp = tick as f64 / 120.0;
            while next_input < inputs.len() && inputs[next_input].timestamp <= timestamp {
                let input = inputs[next_input];
                engine.feed(0.0, input.delta_y, input.timestamp, InputKind::Wheel);
                next_input += 1;
            }
            if let Some(emission) = engine.advance(timestamp) {
                output.push((timestamp, emission));
            }
        }
        output
    }

    fn touch_peak(inputs: &[TimedInput]) -> f64 {
        timed_emissions(inputs)
            .iter()
            .filter(|(_, emission)| {
                matches!(emission.phase, Phase::TouchBegan | Phase::TouchChanged)
            })
            .map(|(_, emission)| emission.delta_y.abs())
            .fold(0.0, f64::max)
    }

    fn total_distance(inputs: &[TimedInput]) -> f64 {
        timed_emissions(inputs)
            .iter()
            .map(|(_, emission)| emission.delta_y.abs())
            .sum()
    }

    fn split_detents(count: usize, interval: f64) -> Vec<TimedInput> {
        const PIECES_PER_DETENT: usize = 8;
        (0..count * PIECES_PER_DETENT)
            .map(|piece| TimedInput {
                timestamp: piece as f64 * interval / PIECES_PER_DETENT as f64,
                delta_y: 36.0 / PIECES_PER_DETENT as f64,
            })
            .collect()
    }

    #[test]
    fn desired_velocity_curve_matches_linear_mouse_formula_and_is_signed() {
        let tuning = AxisTuning::new(settings(SmoothPreset::EaseInOut));
        let input: f64 = 36.0;
        let profile = SmoothPreset::EaseInOut.profile();
        let expected = input
            * (input / (input + 24.0)).powf(profile.input_exponent)
            * profile.velocity_scale
            * (0.85 + 1.02 * 0.4)
            * (1.0 + 1.10 * profile.acceleration_gain);
        assert!((tuning.desired_velocity(input) - expected).abs() < 1e-10);
        assert_eq!(
            tuning.desired_velocity(-input),
            -tuning.desired_velocity(input)
        );
        assert!(tuning.desired_velocity(72.0) > tuning.desired_velocity(36.0));
    }

    #[test]
    fn momentum_decay_is_exponential_and_increases_with_inertia() {
        let mut low = settings(SmoothPreset::EaseInOut);
        low.inertia = 0.0;
        let mut high = low;
        high.inertia = 5.0;
        let dt = 1.0 / 120.0;
        let low_decay = AxisTuning::new(low).momentum_decay(dt);
        let high_decay = AxisTuning::new(high).momentum_decay(dt);
        assert!(low_decay < 1.0 && high_decay < 1.0);
        assert!(high_decay > low_decay);
        assert!(
            (low_decay - (SmoothPreset::EaseInOut.profile().decay - 0.0325).powf(0.5)).abs()
                < 1e-10
        );
    }

    #[test]
    fn same_direction_reengagement_carries_tail_without_exceeding_its_ceiling() {
        let tuning = AxisTuning::new(settings(SmoothPreset::EaseInOut));
        let input_velocity = tuning.desired_velocity(30.0);
        let current = input_velocity * 0.25;
        let reengaged = tuning.reengaged_desired_velocity(30.0, current);
        assert!(reengaged > current);
        assert!(reengaged < input_velocity * 1.12);
        let kick = tuning.reengagement_kick_factor(reengaged, current);
        let blend = tuning.reengagement_blend_factor(1.0 / 120.0, reengaged, current);
        assert!((0.04..=0.24).contains(&kick));
        assert!(blend > 0.0 && blend < tuning.blend_factor(1.0 / 120.0));
    }

    #[test]
    fn opposing_momentum_is_cancelled_and_only_excess_input_restarts() {
        let tuning = AxisTuning::new(settings(SmoothPreset::EaseInOut));
        let current = tuning.desired_velocity(18.0);
        assert_eq!(
            tuning.residual_input_after_cancelling_momentum(-9.0, current),
            0.0
        );
        let excess = tuning.residual_input_after_cancelling_momentum(-90.0, current);
        assert!(excess < 0.0 && excess.abs() < 90.0);
        let opposing_target = tuning.reengaged_desired_velocity(-30.0, current);
        assert!(opposing_target < current);
    }

    #[test]
    fn estimator_projects_dense_wheel_input_but_caps_to_recent_received_distance() {
        let mut estimator = WheelInputVelocityEstimator::default();
        estimator.add(4.0, 0.0);
        estimator.add(4.0, 1.0 / 120.0);
        let projected = estimator.projected_input(4.0, 1.0 / 120.0);
        assert!(projected >= 4.0);
        assert!(projected <= 8.0);
        assert!(estimator.projected_input(0.0, 0.02).abs() == 0.0);
        assert_eq!(estimator.projected_input(4.0, 0.2), 4.0);
    }

    #[test]
    fn dense_split_input_matches_aggregated_input_at_the_same_tick() {
        let mut aggregated = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        let mut split = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        aggregated.feed(0.0, 36.0, 0.0, InputKind::Wheel);
        for step in 0..8 {
            split.feed(0.0, 4.5, step as f64 / 960.0, InputKind::Wheel);
        }
        let aggregated = aggregated.advance(1.0 / 120.0).unwrap();
        let split = split.advance(1.0 / 120.0).unwrap();
        assert!((split.delta_y.abs() - aggregated.delta_y.abs()).abs() < 0.001);
    }

    #[test]
    fn a_new_wheel_session_does_not_reuse_the_previous_idle_tick_time() {
        let mut reused = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        let mut fresh = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        reused.feed(0.0, 36.0, 0.0, InputKind::Wheel);
        for tick in 1..=600 {
            let _ = reused.advance(tick as f64 / 120.0);
            if !reused.is_running() {
                break;
            }
        }
        assert!(!reused.is_running());
        reused.feed(0.0, 36.0, 10.0, InputKind::Wheel);
        fresh.feed(0.0, 36.0, 10.0, InputKind::Wheel);
        let reused = reused.advance(10.0 + 1.0 / 120.0).unwrap();
        let fresh = fresh.advance(10.0 + 1.0 / 120.0).unwrap();
        assert_eq!(reused.phase, Phase::TouchBegan);
        assert_eq!(fresh.phase, Phase::TouchBegan);
        assert!((reused.delta_y.abs() - fresh.delta_y.abs()).abs() < 0.001);
    }

    #[test]
    fn wheel_input_density_changes_peak_and_total_output_without_global_scaling() {
        let slow = split_detents(4, 0.16);
        let medium = split_detents(4, 0.08);
        let fast = split_detents(4, 1.0 / 55.0);
        let slow_peak = touch_peak(&slow);
        let medium_peak = touch_peak(&medium);
        let fast_peak = touch_peak(&fast);
        assert!(medium_peak > slow_peak);
        assert!(fast_peak > medium_peak);
        assert!(fast_peak > slow_peak * 2.0);
        let slow_total = total_distance(&slow);
        let medium_total = total_distance(&medium);
        let fast_total = total_distance(&fast);
        assert!(slow_total > 0.0 && medium_total > slow_total && fast_total > medium_total);
    }

    #[test]
    fn momentum_reengagement_blends_new_input_without_a_sharp_jump() {
        let mut engine = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        let mut latest_momentum = None;
        for step in 0..6 {
            let timestamp = step as f64 / 120.0;
            engine.feed(0.0, 40.0, timestamp, InputKind::Wheel);
            let _ = engine.advance(timestamp + 1.0 / 120.0);
        }
        for step in 6..24 {
            let timestamp = (step + 1) as f64 / 120.0;
            if let Some(emission) = engine.advance(timestamp) {
                if emission.phase == Phase::MomentumChanged {
                    latest_momentum = Some(emission);
                }
            }
        }
        let baseline = latest_momentum.unwrap();
        let reengagement_time = 25.0 / 120.0;
        engine.feed(0.0, 36.0, reengagement_time, InputKind::Wheel);
        let reengaged = engine.advance(reengagement_time + 1.0 / 120.0).unwrap();
        assert_eq!(reengaged.phase, Phase::TouchBegan);
        assert!(reengaged.delta_y.abs() > baseline.delta_y.abs());
        assert!(reengaged.delta_y.abs() < baseline.delta_y.abs() * 2.6);
    }

    #[test]
    fn weak_opposite_reengagement_cancels_momentum_before_a_new_scroll() {
        let mut engine = SmoothEngine::new(settings(SmoothPreset::EaseInOut));
        for step in 0..6 {
            let timestamp = step as f64 / 120.0;
            engine.feed(0.0, 40.0, timestamp, InputKind::Wheel);
            let _ = engine.advance(timestamp + 1.0 / 120.0);
        }
        for step in 6..24 {
            let timestamp = (step + 1) as f64 / 120.0;
            let _ = engine.advance(timestamp);
        }
        let reengagement_time = 25.0 / 120.0;
        engine.feed(0.0, -1.0, reengagement_time, InputKind::Wheel);
        let cancelled = engine.advance(reengagement_time + 1.0 / 120.0).unwrap();
        assert_eq!(cancelled.phase, Phase::MomentumEnded);
        assert!(cancelled.delta_y.abs() < 0.001);
        engine.feed(
            0.0,
            -36.0,
            reengagement_time + 2.0 / 120.0,
            InputKind::Wheel,
        );
        let reengaged = engine.advance(reengagement_time + 3.0 / 120.0).unwrap();
        assert_eq!(reengaged.phase, Phase::TouchBegan);
        assert!(reengaged.delta_y < 0.0);
    }

    #[test]
    fn phase_sequence_emits_touch_then_momentum_began_changed_and_ended() {
        let mut engine = SmoothEngine::new(settings(SmoothPreset::Linear));
        engine.feed(0.0, 36.0, 0.0, InputKind::Wheel);
        let first = collect(&mut engine, 1.0 / 120.0, 4.0 / 120.0, 1.0 / 120.0);
        assert_eq!(first.first().unwrap().phase, Phase::TouchBegan);
        assert!(first
            .iter()
            .skip(1)
            .all(|emission| emission.phase == Phase::TouchChanged));
        let tail = collect(&mut engine, 0.5, 4.0, 1.0 / 120.0);
        assert!(tail
            .iter()
            .any(|emission| emission.phase == Phase::TouchEnded));
        assert!(tail
            .iter()
            .any(|emission| emission.phase == Phase::MomentumBegan));
        assert!(tail
            .iter()
            .any(|emission| emission.phase == Phase::MomentumChanged));
        assert_eq!(tail.last().unwrap().phase, Phase::MomentumEnded);
        assert!(!engine.is_running());
    }

    #[test]
    fn wheel_estimator_and_engine_reset_on_direction_change_and_long_gap() {
        let mut estimator = WheelInputVelocityEstimator::default();
        estimator.add(8.0, 0.0);
        estimator.add(-2.0, 0.005);
        assert_eq!(estimator.direction, -1);
        assert_eq!(estimator.recent_inputs.len(), 1);
        estimator.projected_input(0.0, 0.1);
        assert_eq!(estimator.direction, 0);
        assert_eq!(estimator.rate_adjusted_input, 0.0);
    }

    #[test]
    fn passthrough_axis_preserves_input_while_smoothed_axis_runs() {
        let mut engine = SmoothEngine::new(settings(SmoothPreset::Smooth));
        engine.horizontal_behavior = AxisBehavior::Passthrough;
        engine.feed(2.0, 36.0, 0.0, InputKind::Wheel);
        let emission = engine.advance(1.0 / 120.0).unwrap();
        assert_eq!(emission.delta_x, 2.0);
        assert_ne!(emission.delta_y, 2.0);
    }

    #[test]
    fn reset_other_axis_clears_an_exclusive_tail() {
        let mut engine = SmoothEngine::new(settings(SmoothPreset::Smooth));
        engine.feed(0.0, 36.0, 0.0, InputKind::Wheel);
        let _ = engine.advance(1.0 / 120.0);
        assert_eq!(engine.exclusive_active_axis(), Some(Axis::Vertical));
        engine.reset_other_axis(Axis::Horizontal);
        assert_eq!(engine.velocity_y, 0.0);
    }
}
