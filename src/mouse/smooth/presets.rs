// MIT License
// Copyright (c) 2021-2024 LinearMouse

//! Preset curves and user-facing tuning values ported from LinearMouse.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PresetProfile {
    pub response: f64,
    pub input_exponent: f64,
    pub acceleration_gain: f64,
    pub decay: f64,
    pub velocity_scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SmoothSettings {
    pub enabled: bool,
    pub preset: SmoothPreset,
    pub response: f64,
    pub speed: f64,
    pub acceleration: f64,
    pub inertia: f64,
}

impl Default for SmoothSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: SmoothPreset::EaseInOut,
            response: 0.68,
            speed: 1.02,
            acceleration: 1.10,
            inertia: 0.74,
        }
    }
}

impl SmoothPreset {
    pub(crate) const ALL: [Self; 13] = [
        Self::EaseInOut,
        Self::EaseIn,
        Self::EaseOut,
        Self::Linear,
        Self::Quadratic,
        Self::Cubic,
        Self::EaseOutCubic,
        Self::EaseInOutCubic,
        Self::Quartic,
        Self::EaseOutQuartic,
        Self::EaseInOutQuartic,
        Self::Smooth,
        Self::Custom,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Custom => "custom",
            Self::Linear => "linear",
            Self::EaseIn => "ease_in",
            Self::EaseOut => "ease_out",
            Self::EaseInOut => "ease_in_out",
            Self::Quadratic => "quadratic",
            Self::Cubic => "cubic",
            Self::Quartic => "quartic",
            Self::EaseOutCubic => "ease_out_cubic",
            Self::EaseInOutCubic => "ease_in_out_cubic",
            Self::EaseOutQuartic => "ease_out_quartic",
            Self::EaseInOutQuartic => "ease_in_out_quartic",
            Self::Smooth => "smooth",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|preset| preset.as_str() == value)
    }

    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Custom => "mouse_smooth_preset_custom",
            Self::Linear => "mouse_smooth_preset_linear",
            Self::EaseIn => "mouse_smooth_preset_ease_in",
            Self::EaseOut => "mouse_smooth_preset_ease_out",
            Self::EaseInOut => "mouse_smooth_preset_ease_in_out",
            Self::Quadratic => "mouse_smooth_preset_quadratic",
            Self::Cubic => "mouse_smooth_preset_cubic",
            Self::Quartic => "mouse_smooth_preset_quartic",
            Self::EaseOutCubic => "mouse_smooth_preset_ease_out_cubic",
            Self::EaseInOutCubic => "mouse_smooth_preset_ease_in_out_cubic",
            Self::EaseOutQuartic => "mouse_smooth_preset_ease_out_quartic",
            Self::EaseInOutQuartic => "mouse_smooth_preset_ease_in_out_quartic",
            Self::Smooth => "mouse_smooth_preset_smooth",
        }
    }

    pub(crate) fn profile(self) -> PresetProfile {
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

    /// Slider values shown when a preset is selected, copied from LinearMouse's defaults.
    pub(crate) fn default_settings(self) -> (f64, f64, f64, f64) {
        match self {
            Self::Custom => (0.68, 1.00, 1.00, 0.80),
            Self::Linear => (0.92, 1.00, 0.78, 0.44),
            Self::EaseIn => (0.38, 0.92, 0.86, 1.00),
            Self::EaseOut => (0.88, 1.02, 0.94, 0.42),
            Self::EaseInOut => (0.68, 1.02, 1.10, 0.74),
            Self::Quadratic => (0.58, 1.04, 1.18, 0.72),
            Self::Cubic => (0.54, 1.08, 1.24, 0.76),
            Self::Quartic => (0.48, 1.12, 1.32, 0.82),
            Self::EaseOutCubic => (0.94, 1.06, 0.92, 0.42),
            Self::EaseInOutCubic => (0.62, 1.06, 1.20, 0.78),
            Self::EaseOutQuartic => (0.98, 1.10, 0.90, 0.38),
            Self::EaseInOutQuartic => (0.56, 1.10, 1.28, 0.82),
            Self::Smooth => (0.80, 1.00, 0.88, 0.92),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_catalogue_and_profiles_match_linear_mouse() {
        let expected = [
            (
                SmoothPreset::EaseInOut,
                PresetProfile {
                    response: 0.68,
                    input_exponent: 1.06,
                    acceleration_gain: 0.10,
                    decay: 0.89,
                    velocity_scale: 31.0,
                },
                (0.68, 1.02, 1.10, 0.74),
            ),
            (
                SmoothPreset::EaseIn,
                PresetProfile {
                    response: 0.34,
                    input_exponent: 1.18,
                    acceleration_gain: 0.08,
                    decay: 0.93,
                    velocity_scale: 24.0,
                },
                (0.38, 0.92, 0.86, 1.00),
            ),
            (
                SmoothPreset::EaseOut,
                PresetProfile {
                    response: 0.90,
                    input_exponent: 0.92,
                    acceleration_gain: 0.08,
                    decay: 0.84,
                    velocity_scale: 34.0,
                },
                (0.88, 1.02, 0.94, 0.42),
            ),
            (
                SmoothPreset::Linear,
                PresetProfile {
                    response: 0.94,
                    input_exponent: 0.96,
                    acceleration_gain: 0.04,
                    decay: 0.83,
                    velocity_scale: 34.0,
                },
                (0.92, 1.00, 0.78, 0.44),
            ),
            (
                SmoothPreset::Quadratic,
                PresetProfile {
                    response: 0.58,
                    input_exponent: 1.12,
                    acceleration_gain: 0.12,
                    decay: 0.88,
                    velocity_scale: 33.0,
                },
                (0.58, 1.04, 1.18, 0.72),
            ),
            (
                SmoothPreset::Cubic,
                PresetProfile {
                    response: 0.52,
                    input_exponent: 1.18,
                    acceleration_gain: 0.14,
                    decay: 0.89,
                    velocity_scale: 35.0,
                },
                (0.54, 1.08, 1.24, 0.76),
            ),
            (
                SmoothPreset::EaseOutCubic,
                PresetProfile {
                    response: 0.94,
                    input_exponent: 0.86,
                    acceleration_gain: 0.08,
                    decay: 0.82,
                    velocity_scale: 35.0,
                },
                (0.94, 1.06, 0.92, 0.42),
            ),
            (
                SmoothPreset::EaseInOutCubic,
                PresetProfile {
                    response: 0.62,
                    input_exponent: 1.12,
                    acceleration_gain: 0.12,
                    decay: 0.89,
                    velocity_scale: 33.0,
                },
                (0.62, 1.06, 1.20, 0.78),
            ),
            (
                SmoothPreset::Quartic,
                PresetProfile {
                    response: 0.46,
                    input_exponent: 1.24,
                    acceleration_gain: 0.16,
                    decay: 0.90,
                    velocity_scale: 37.0,
                },
                (0.48, 1.12, 1.32, 0.82),
            ),
            (
                SmoothPreset::EaseOutQuartic,
                PresetProfile {
                    response: 0.98,
                    input_exponent: 0.80,
                    acceleration_gain: 0.08,
                    decay: 0.80,
                    velocity_scale: 36.0,
                },
                (0.98, 1.10, 0.90, 0.38),
            ),
            (
                SmoothPreset::EaseInOutQuartic,
                PresetProfile {
                    response: 0.56,
                    input_exponent: 1.18,
                    acceleration_gain: 0.14,
                    decay: 0.90,
                    velocity_scale: 34.0,
                },
                (0.56, 1.10, 1.28, 0.82),
            ),
            (
                SmoothPreset::Smooth,
                PresetProfile {
                    response: 0.80,
                    input_exponent: 0.98,
                    acceleration_gain: 0.06,
                    decay: 0.93,
                    velocity_scale: 33.0,
                },
                (0.80, 1.00, 0.88, 0.92),
            ),
            (
                SmoothPreset::Custom,
                PresetProfile {
                    response: 0.64,
                    input_exponent: 1.00,
                    acceleration_gain: 0.10,
                    decay: 0.89,
                    velocity_scale: 32.0,
                },
                (0.68, 1.00, 1.00, 0.80),
            ),
        ];
        assert_eq!(SmoothPreset::ALL.len(), expected.len());
        for (index, (preset, profile, defaults)) in expected.into_iter().enumerate() {
            assert_eq!(SmoothPreset::ALL[index], preset, "preset order at {index}");
            assert_eq!(preset.profile(), profile, "profile table for {preset:?}");
            assert_eq!(
                preset.default_settings(),
                defaults,
                "slider defaults for {preset:?}"
            );
        }
        assert_eq!(SmoothPreset::ALL[0], SmoothPreset::EaseInOut);
        assert_eq!(SmoothPreset::ALL[12], SmoothPreset::Custom);
    }

    #[test]
    fn preset_names_round_trip() {
        for preset in SmoothPreset::ALL {
            assert_eq!(SmoothPreset::from_str(preset.as_str()), Some(preset));
        }
        assert_eq!(SmoothPreset::from_str("unknown"), None);
    }
}
