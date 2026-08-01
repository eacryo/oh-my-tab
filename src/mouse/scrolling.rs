//! 滚轮滚动模式:默认(透传+可反转)/按行(固定行数)/平滑(物理引擎+惯性)。
//! 平滑引擎是三态状态机(Idle → Active → Momentum),被 120Hz CFRunLoopTimer 驱动。
//!
//! Scroll modes: Default (passthrough + optional reverse) / Line (fixed line count) / Smooth
//! (physics engine + inertia). The smooth engine is a three-state machine (Idle → Active →
//! Momentum) driven by a 120Hz CFRunLoopTimer.

use std::sync::Mutex;
use std::time::Instant;

// ========== 滚动模式 / Scroll mode ==========

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum ScrollMode {
    Default,
    Line,
    Smooth,
}

impl ScrollMode {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "line" => Self::Line,
            "smooth" => Self::Smooth,
            _ => Self::Default,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Line => "line",
            Self::Smooth => "smooth",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn current() -> Self {
        // 兼容旧路径(无设备上下文):用"所有鼠标"解析。
        // Legacy path (no device context): resolve with the "All Mice" profile.
        let r = crate::mouse::resolve::resolve(None);
        r.scroll_mode
    }

    #[allow(dead_code)]
    pub(crate) fn all_labels() -> &'static [&'static str] {
        &["default", "line", "smooth"]
    }
}

// ========== 平滑预设 / Smooth preset ==========

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SmoothPreset {
    Custom,
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Quadratic,
    Cubic,
    Quartic,
    EaseOutCubic,
    EaseInOutCubic,
    EaseOutQuartic,
    EaseInOutQuartic,
    Smooth,
}

/// 预设的五参数配置。
/// Five-parameter preset profile.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PresetProfile {
    pub(crate) response: f64,
    pub(crate) input_exponent: f64,
    pub(crate) acceleration_gain: f64,
    pub(crate) decay: f64,
    pub(crate) velocity_scale: f64,
}

impl SmoothPreset {
    pub(crate) fn from_str(s: &str) -> Self {
        match s {
            "custom" => Self::Custom,
            "linear" => Self::Linear,
            "easeIn" => Self::EaseIn,
            "easeOut" => Self::EaseOut,
            "easeInOut" => Self::EaseInOut,
            "quadratic" => Self::Quadratic,
            "cubic" => Self::Cubic,
            "quartic" => Self::Quartic,
            "easeOutCubic" => Self::EaseOutCubic,
            "easeInOutCubic" => Self::EaseInOutCubic,
            "easeOutQuartic" => Self::EaseOutQuartic,
            "easeInOutQuartic" => Self::EaseInOutQuartic,
            "smooth" => Self::Smooth,
            _ => Self::EaseInOut,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Linear => "linear",
            Self::EaseIn => "easeIn",
            Self::EaseOut => "easeOut",
            Self::EaseInOut => "easeInOut",
            Self::Quadratic => "quadratic",
            Self::Cubic => "cubic",
            Self::Quartic => "quartic",
            Self::EaseOutCubic => "easeOutCubic",
            Self::EaseInOutCubic => "easeInOutCubic",
            Self::EaseOutQuartic => "easeOutQuartic",
            Self::EaseInOutQuartic => "easeInOutQuartic",
            Self::Smooth => "smooth",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn all_labels() -> Vec<&'static str> {
        vec![
            "easeInOut",
            "easeIn",
            "easeOut",
            "linear",
            "quadratic",
            "cubic",
            "easeOutCubic",
            "easeInOutCubic",
            "quartic",
            "easeOutQuartic",
            "easeInOutQuartic",
            "smooth",
            "custom",
        ]
    }

    /// 返回预设的 5 参数配置。直接从 LinearMouse Smoothed.swift 移植。
    /// Returns the 5-parameter profile, ported from LinearMouse Smoothed.swift.
    pub(crate) fn profile(&self) -> PresetProfile {
        match self {
            Self::Custom => PresetProfile {
                response: 0.64,
                input_exponent: 1.00,
                acceleration_gain: 0.10,
                decay: 0.89,
                velocity_scale: 32.0,
            },
            Self::Linear => PresetProfile {
                response: 0.94,
                input_exponent: 0.96,
                acceleration_gain: 0.04,
                decay: 0.83,
                velocity_scale: 34.0,
            },
            Self::EaseIn => PresetProfile {
                response: 0.34,
                input_exponent: 1.18,
                acceleration_gain: 0.08,
                decay: 0.93,
                velocity_scale: 24.0,
            },
            Self::EaseOut => PresetProfile {
                response: 0.90,
                input_exponent: 0.92,
                acceleration_gain: 0.08,
                decay: 0.84,
                velocity_scale: 34.0,
            },
            Self::EaseInOut => PresetProfile {
                response: 0.68,
                input_exponent: 1.06,
                acceleration_gain: 0.10,
                decay: 0.89,
                velocity_scale: 31.0,
            },
            Self::Quadratic => PresetProfile {
                response: 0.58,
                input_exponent: 1.12,
                acceleration_gain: 0.12,
                decay: 0.88,
                velocity_scale: 33.0,
            },
            Self::Cubic => PresetProfile {
                response: 0.52,
                input_exponent: 1.18,
                acceleration_gain: 0.14,
                decay: 0.89,
                velocity_scale: 35.0,
            },
            Self::Quartic => PresetProfile {
                response: 0.46,
                input_exponent: 1.24,
                acceleration_gain: 0.16,
                decay: 0.90,
                velocity_scale: 37.0,
            },
            Self::EaseOutCubic => PresetProfile {
                response: 0.94,
                input_exponent: 0.86,
                acceleration_gain: 0.08,
                decay: 0.82,
                velocity_scale: 35.0,
            },
            Self::EaseInOutCubic => PresetProfile {
                response: 0.62,
                input_exponent: 1.12,
                acceleration_gain: 0.12,
                decay: 0.89,
                velocity_scale: 33.0,
            },
            Self::EaseOutQuartic => PresetProfile {
                response: 0.98,
                input_exponent: 0.80,
                acceleration_gain: 0.08,
                decay: 0.80,
                velocity_scale: 36.0,
            },
            Self::EaseInOutQuartic => PresetProfile {
                response: 0.56,
                input_exponent: 1.18,
                acceleration_gain: 0.14,
                decay: 0.90,
                velocity_scale: 34.0,
            },
            Self::Smooth => PresetProfile {
                response: 0.80,
                input_exponent: 0.98,
                acceleration_gain: 0.06,
                decay: 0.93,
                velocity_scale: 33.0,
            },
        }
    }
}

// ========== 平滑引擎 / Smooth engine ==========

/// Engine internal state.
#[derive(Debug, Clone, Copy, PartialEq)]
enum EnginePhase {
    /// No active scrolling or momentum.
    Idle,
    /// User is actively scrolling (receiving input).
    Active,
    /// User has stopped, velocity decaying.
    Momentum,
}

/// Emission from one advance tick.
#[derive(Debug)]
pub(crate) struct TickEmission {
    pub(crate) delta_x: f64,
    pub(crate) delta_y: f64,
    /// Scroll phase: 0=none, 1=began, 2=changed, 4=ended
    pub(crate) scroll_phase: u32,
    /// Momentum phase: 0=none, 1=began, 2=changed, 3=ended
    pub(crate) momentum_phase: u32,
}

/// 平滑滚动引擎(简化版三态状态机)。
/// 移植自 LinearMouse SmoothedScrollingEngine + ScrollingEngine 的核心算法,
/// 省略了 reengagement dominance/tail recovery 等精细调校。
///
/// Simplified three-state smooth scrolling engine.
/// Ported from LinearMouse's SmoothedScrollingEngine + SmoothedScrollingEngine core
/// algorithms, omitting reengagement dominance/tail recovery for simplicity.
pub(crate) struct SmoothedEngine {
    phase: EnginePhase,
    profile: PresetProfile,
    preset: SmoothPreset,

    velocity_x: f64,
    velocity_y: f64,
    desired_x: f64,
    desired_y: f64,

    pending_x: f64,
    pending_y: f64,

    last_tick: Instant,
    last_input: Option<Instant>,

    /// Whether touchBegan has been emitted this session.
    touch_began: bool,
    /// Whether momentumBegan is pending for next tick.
    pending_momentum_began: bool,
}

impl SmoothedEngine {
    pub(crate) fn new(preset: SmoothPreset) -> Self {
        let profile = preset.profile();
        Self {
            phase: EnginePhase::Idle,
            profile,
            preset,
            velocity_x: 0.0,
            velocity_y: 0.0,
            desired_x: 0.0,
            desired_y: 0.0,
            pending_x: 0.0,
            pending_y: 0.0,
            last_tick: Instant::now(),
            last_input: None,
            touch_began: false,
            pending_momentum_began: false,
        }
    }

    /// Reset engine to idle (e.g. on mode switch).
    #[allow(dead_code)]
    pub(crate) fn reset(&mut self) {
        self.phase = EnginePhase::Idle;
        self.velocity_x = 0.0;
        self.velocity_y = 0.0;
        self.desired_x = 0.0;
        self.desired_y = 0.0;
        self.pending_x = 0.0;
        self.pending_y = 0.0;
        self.last_tick = Instant::now();
        self.last_input = None;
        self.touch_began = false;
        self.pending_momentum_began = false;
    }

    /// Reconfigure with a new preset (when config changes).
    pub(crate) fn set_preset(&mut self, preset: SmoothPreset) {
        self.preset = preset;
        self.profile = preset.profile();
    }

    /// Feed discrete wheel input into the engine.
    pub(crate) fn feed(&mut self, dy: f64, dx: f64) {
        let now = Instant::now();

        self.pending_x += dx;
        self.pending_y += dy;

        let has_input = dx != 0.0 || dy != 0.0;
        if has_input {
            self.last_input = Some(now);
        }

        match self.phase {
            EnginePhase::Idle if has_input => {
                self.phase = EnginePhase::Active;
                self.touch_began = false;
                self.pending_momentum_began = false;
                self.last_tick = now;
            }
            EnginePhase::Momentum if has_input => {
                self.phase = EnginePhase::Active;
                self.touch_began = false;
                self.pending_momentum_began = false;
            }
            _ => {}
        }
    }

    /// Advance the engine by one tick (~8.3ms at 120Hz).
    /// Returns the emission to post, or None if nothing to emit.
    pub(crate) fn advance(&mut self) -> Option<TickEmission> {
        let now = Instant::now();
        let dt = (now - self.last_tick)
            .as_secs_f64()
            .clamp(1.0 / 240.0, 1.0 / 24.0);
        self.last_tick = now;

        // 120Hz firing rate: dt is normally ~0.0083s.
        // 120Hz firing rate: dt is normally ~0.0083s.

        let has_pending = self.pending_x != 0.0 || self.pending_y != 0.0;
        let fresh_input = self
            .last_input
            .map(|t| (now - t).as_secs_f64() < 1.0 / 25.0)
            .unwrap_or(false);

        match self.phase {
            EnginePhase::Idle => {
                self.pending_x = 0.0;
                self.pending_y = 0.0;
                None
            }

            EnginePhase::Active => {
                if has_pending {
                    // 根据 pending input 计算期望速度
                    // Compute desired velocity from pending input
                    let eff_x = self.effective_input(self.pending_x);
                    let eff_y = self.effective_input(self.pending_y);
                    self.desired_x = self.desired_velocity(eff_x);
                    self.desired_y = self.desired_velocity(eff_y);
                    self.pending_x = 0.0;
                    self.pending_y = 0.0;
                }

                if fresh_input {
                    // 有新鲜输入:blend 速度 -> emit touchBegan/Changed
                    // Fresh input: blend velocity → emit touchBegan/Changed
                    let blend = self.blend_factor(dt);
                    self.velocity_x += (self.desired_x - self.velocity_x) * blend;
                    self.velocity_y += (self.desired_y - self.velocity_y) * blend;

                    let emission = self.emission_from_velocity();
                    let phase = if !self.touch_began {
                        self.touch_began = true;
                        1
                    } else {
                        2
                    };
                    Some(TickEmission {
                        delta_x: emission.0,
                        delta_y: emission.1,
                        scroll_phase: phase,
                        momentum_phase: 0,
                    })
                } else if self.running() {
                    // 无新鲜输入但速度 > 阈值:进入 momentum
                    // No fresh input but velocity > threshold: enter momentum
                    self.phase = EnginePhase::Momentum;
                    self.pending_momentum_began = true;
                    self.touch_began = false;
                    Some(TickEmission {
                        delta_x: 0.0,
                        delta_y: 0.0,
                        scroll_phase: 4, // ended
                        momentum_phase: 0,
                    })
                } else {
                    // 无新鲜输入且速度很小:回到 idle
                    // No fresh input and velocity tiny: back to idle
                    self.phase = EnginePhase::Idle;
                    self.touch_began = false;
                    self.velocity_x = 0.0;
                    self.velocity_y = 0.0;
                    self.desired_x = 0.0;
                    self.desired_y = 0.0;
                    Some(TickEmission {
                        delta_x: 0.0,
                        delta_y: 0.0,
                        scroll_phase: 4, // ended
                        momentum_phase: 0,
                    })
                }
            }

            EnginePhase::Momentum => {
                // 惯性衰减
                // Inertial decay
                let decay = self.momentum_decay(dt);
                self.velocity_x *= decay;
                self.velocity_y *= decay;

                if self.running() {
                    let emission = self.emission_from_velocity();
                    if self.pending_momentum_began {
                        self.pending_momentum_began = false;
                        Some(TickEmission {
                            delta_x: emission.0,
                            delta_y: emission.1,
                            scroll_phase: 0,
                            momentum_phase: 1, // began
                        })
                    } else {
                        Some(TickEmission {
                            delta_x: emission.0,
                            delta_y: emission.1,
                            scroll_phase: 0,
                            momentum_phase: 2, // changed
                        })
                    }
                } else {
                    // 速度低于阈值:回到 idle
                    // Velocity below threshold: back to idle
                    self.phase = EnginePhase::Idle;
                    self.touch_began = false;
                    self.pending_momentum_began = false;
                    self.velocity_x = 0.0;
                    self.velocity_y = 0.0;
                    self.desired_x = 0.0;
                    self.desired_y = 0.0;
                    Some(TickEmission {
                        delta_x: 0.0,
                        delta_y: 0.0,
                        scroll_phase: 0,
                        momentum_phase: 3, // ended
                    })
                }
            }
        }
    }

    /// Whether the engine has meaningful velocity (above stop threshold).
    fn running(&self) -> bool {
        self.velocity_x.abs() > 0.5 || self.velocity_y.abs() > 0.5
    }

    /// Convert velocity to per-frame delta (velocity * dt).
    fn emission_from_velocity(&self) -> (f64, f64) {
        let dt = (Instant::now() - self.last_tick)
            .as_secs_f64()
            .max(1.0 / 240.0);
        (self.velocity_x * dt, self.velocity_y * dt)
    }

    /// Compute the desired velocity for a given input delta.
    /// Uses the LinearMouse formula:
    ///   normalized = |input| / (|input| + 24)
    ///   curved = pow(normalized, input_exponent)
    ///   magnitude = |input| * curved * velocity_scale
    ///   speed_boost = 0.85 + speed_factor * 0.4  (speed_factor hardcoded to 1.0 for now)
    ///   accel_boost = 1 + acceleration * acceleration_gain (acceleration hardcoded to 1.0 for now)
    ///   velocity = magnitude * speed_boost * accel_boost
    fn desired_velocity(&self, input: f64) -> f64 {
        if input == 0.0 {
            return 0.0;
        }
        let sign = input.signum();
        let base_mag = input.abs();
        let normalized = (base_mag / (base_mag + 24.0)).clamp(0.0, 1.0);
        let curved = normalized.powf(self.profile.input_exponent);
        let mag = base_mag * curved;
        let speed_boost = 0.85 + 1.0 * 0.4; // speed=1.0 (default)
        let accel_boost = 1.0 + 1.0 * self.profile.acceleration_gain; // acceleration=1.0 (default)
        sign * mag * self.profile.velocity_scale * speed_boost * accel_boost
    }

    /// Blend factor: how fast velocity catches up to desired velocity.
    fn blend_factor(&self, dt: f64) -> f64 {
        let scaled = self.profile.response * 0.75 + 0.68 * 0.8;
        (scaled * dt * 60.0).clamp(0.0, 1.0)
    }

    /// Momentum decay factor per tick.
    fn momentum_decay(&self, dt: f64) -> f64 {
        let dt_scale = (dt * 60.0).max(0.25);
        let decay = self.profile.decay.clamp(0.72, 0.98);
        decay.powf(dt_scale)
    }

    /// Effective input, capped by rate estimation (simplified — use raw pending).
    fn effective_input(&self, pending: f64) -> f64 {
        pending
    }
}

// ========== 全局引擎(per-device)/ Global engine (per-device) ==========

/// per-device 平滑引擎映射。key = (VID, PID);None 键 = 归因失败回退(共用一个引擎)。
/// 由 event_tap 线程独占访问,Mutex 仅用于安全,无竞争。
///
/// Per-device smooth-engine map. Key = (VID, PID); the None key = attribution-failure fallback
/// (shares one engine). Accessed exclusively by the event tap thread; Mutex is for safety.
pub(crate) static SMOOTH_ENGINES: std::sync::LazyLock<
    Mutex<std::collections::HashMap<Option<crate::mouse::device::DeviceKey>, SmoothedEngine>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 取/建指定设备的引擎,并喂入输入。
/// Get-or-create the engine for the given device and feed it input.
pub(crate) fn feed_engine(
    device: Option<crate::mouse::device::DeviceKey>,
    dy: f64,
    dx: f64,
    preset: SmoothPreset,
) {
    if let Ok(mut engines) = SMOOTH_ENGINES.lock() {
        let e = engines.entry(device).or_insert_with(|| SmoothedEngine::new(preset));
        // 预设可能随配置变化,确保引擎用的是当前预设。
        // The preset may change with config; keep the engine on the current preset.
        e.set_preset(preset);
        e.feed(dy, dx);
    }
}

/// 推进所有活跃引擎一次(由 120Hz 定时器调用),返回需要 post 的事件(如果有)。
/// 当前策略:推进 last-fed 的引擎(简化:同一时刻通常只有一个设备在滚)。
///
/// Advance all active engines by one tick (called by the 120Hz timer); returns an emission if any.
/// Current strategy: advance the most-recently-fed engine (simplification: typically only one
/// device scrolls at a time).
pub(crate) fn advance_engine() -> Option<TickEmission> {
    if let Ok(mut engines) = SMOOTH_ENGINES.lock() {
        // 推进所有引擎,取第一个有发射的。
        // Advance all engines, return the first emission.
        for e in engines.values_mut() {
            if let Some(em) = e.advance() {
                return Some(em);
            }
        }
    }
    None
}

/// 清理已断开设备的引擎条目(重枚举时调用)。
/// Purge engine entries for disconnected devices (called on re-enumeration).
#[allow(dead_code)]
pub(crate) fn purge_stale_engines(active: &[crate::mouse::device::DeviceKey]) {
    if let Ok(mut engines) = SMOOTH_ENGINES.lock() {
        engines.retain(|k, _| {
            k.is_none()
                || k.and_then(|key| active.iter().find(|a| **a == key).copied())
                    .is_some()
        });
    }
}

/// 根据解析后的配置计算要 post 的滚动 delta (非平滑模式用)。
/// 处理反转 + 行模式的行数归一化。结果形参供 post_scroll_event 使用。
///
/// Compute the scroll delta to post (non-smooth modes) from the resolved config.
/// Handles reversal + line-mode normalization.
pub(crate) fn compute_delta(dy: i64, dx: i64, r: &crate::mouse::resolve::ResolvedMouse) -> (i32, i32) {
    let mode = r.scroll_mode;
    let reverse = r.reverse_scroll;

    let (mut ndy, mut ndx) = match mode {
        ScrollMode::Default => (dy as i32, dx as i32),
        ScrollMode::Line => {
            let line_count = r.line_count.clamp(1, 10) as i64;
            let sign_y = if dy != 0 { dy.signum() } else { 0 };
            let sign_x = if dx != 0 { dx.signum() } else { 0 };
            ((sign_y * line_count) as i32, (sign_x * line_count) as i32)
        }
        ScrollMode::Smooth => {
            // 平滑模式不在此 compute;由 post_scroll_smooth 处理
            // Smooth mode is handled separately; not computed here.
            return (0, 0);
        }
    };

    if reverse {
        ndy = -ndy;
        ndx = -ndx;
    }

    (ndy, ndx)
}
