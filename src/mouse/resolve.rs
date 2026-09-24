//! Config resolution: merge the "All Mice" profile + per-device profiles into the effective
//! config (Phase 3). Merge semantics: iterate CONFIG.mouse.profiles; for each matching profile
//! (no device = wildcard, device = VID+PID equality) fold its Some fields into the result,
//! later profiles winning. The "All Mice" profile usually comes first, per-device ones after.

use crate::config::{Config, MouseProfile, CONFIG};
use crate::mouse::device::DeviceKey;
use crate::mouse::scrolling::ScrollMode;
use std::collections::HashMap;
use std::sync::Mutex;

/// Resolved effective config (non-Option; all fields are concrete).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedMouse {
    pub reverse_scroll: bool,
    pub scroll_mode: ScrollMode,
    pub line_count: u32,
    pub disable_acceleration: bool,
    // Pointer acceleration / tracking speed (0..=40); None = leave the device value alone.
    pub acceleration: Option<f64>,
    // Button mappings: button number -> shortcut description (per-key merge, later wins).
    pub button_mappings: HashMap<String, String>,
    // The button-mappings master switch (independent per device; mappings skipped when off).
    pub button_mappings_enabled: bool,
}

impl Default for ResolvedMouse {
    fn default() -> Self {
        Self {
            reverse_scroll: false,
            scroll_mode: ScrollMode::Default,
            line_count: 3,
            disable_acceleration: false,
            acceleration: None,
            button_mappings: HashMap::new(),
            button_mappings_enabled: true,
        }
    }
}

/// Resolve cache: key = (VID, PID); the None key = "no device / All Mice". Invalidated on
/// reload_config and device changes.
static CACHE: std::sync::LazyLock<
    Mutex<std::collections::HashMap<Option<DeviceKey>, ResolvedMouse>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Invalidate the cache (called on config reload or device changes).
pub(crate) fn invalidate_cache() {
    if let Ok(mut c) = CACHE.lock() {
        c.clear();
    }
}

/// Whether a matcher matches the given device. A None key = wildcard (matches all devices,
/// i.e. the "All Mice" profile). The settings page finds profiles through this too
/// (settings::find_profile_index) so resolution and the UI lookup can't drift apart.
pub(crate) fn matches(profile: &MouseProfile, device: Option<DeviceKey>) -> bool {
    // Virtual-pointer profile (injected = true): matches injected events only, never a real device.
    if profile.device.is_virtual() {
        return device == Some(crate::mouse::device::VIRTUAL_DEVICE_KEY);
    }
    // Injected events: apart from the virtual profile only the wildcard ("All Mice") layer merges
    // in, serving as the base.
    if device == Some(crate::mouse::device::VIRTUAL_DEVICE_KEY) {
        return profile.device.vendor_id.is_none() && profile.device.product_id.is_none();
    }
    let Some((vid, pid)) = device else {
        // No device (attribution-failure fallback): match only wildcard profiles.
        return profile.device.vendor_id.is_none() && profile.device.product_id.is_none();
    };
    let vid_ok = profile.device.vendor_id.map(|v| v == vid).unwrap_or(true);
    let pid_ok = profile.device.product_id.map(|p| p == pid).unwrap_or(true);
    vid_ok && pid_ok
}

/// Resolve the effective config for a device. device = None means attribution failed; only the
/// "All Mice" profile applies.
pub(crate) fn resolve(device: Option<DeviceKey>) -> ResolvedMouse {
    // Check the cache.
    if let Ok(c) = CACHE.lock() {
        if let Some(r) = c.get(&device) {
            return r.clone();
        }
    }

    let cfg = CONFIG.read().unwrap().clone();
    let r = resolve_from(&cfg, device);

    if let Ok(mut c) = CACHE.lock() {
        c.insert(device, r.clone());
    }
    r
}

/// Resolve a device's effective config from a given Config (for non-CONFIG contexts like the
/// restore-defaults preview).
pub(crate) fn resolve_from_config(
    cfg: &crate::config::Config,
    device: Option<DeviceKey>,
) -> ResolvedMouse {
    resolve_from(cfg, device)
}

/// Resolve from a given Config (for tests and CONFIG-free scenarios).
fn resolve_from(cfg: &Config, device: Option<DeviceKey>) -> ResolvedMouse {
    let mut r = ResolvedMouse::default();

    // Start from code defaults so every field is concrete.
    let defaults = ResolvedMouse::default();
    r.reverse_scroll = defaults.reverse_scroll;
    r.scroll_mode = defaults.scroll_mode;
    r.line_count = defaults.line_count;
    r.disable_acceleration = defaults.disable_acceleration;
    r.acceleration = defaults.acceleration;
    r.button_mappings = HashMap::new();
    r.button_mappings_enabled = defaults.button_mappings_enabled;

    // Iterate profiles, merging all matching ones (later wins).
    for p in &cfg.mouse.profiles {
        if !matches(p, device) {
            continue;
        }
        if let Some(rs) = p.reverse_scroll {
            r.reverse_scroll = rs;
        }
        if let Some(ref mode) = p.scroll_mode {
            r.scroll_mode = ScrollMode::from_str(mode);
        }
        if let Some(lc) = p.line_count {
            r.line_count = lc.clamp(1, 10);
        }
        if let Some(ref ptr) = p.pointer {
            if let Some(da) = ptr.disable_acceleration {
                r.disable_acceleration = da;
            }
            // Pointer acceleration / tracking speed: later wins (same as every other field).
            if let Some(acc) = ptr.acceleration {
                r.acceleration = Some(acc);
            }
        }
        // Button mappings: fold in per key (same key: later wins).
        for (btn, desc) in &p.button_mappings {
            r.button_mappings.insert(btn.clone(), desc.clone());
        }
        if let Some(en) = p.button_mappings_enabled {
            r.button_mappings_enabled = en;
        }
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_button_mappings_enabled_per_device() {
        let mut cfg = crate::config::Config::default();
        // Default layer: enabled unset (inherits true).
        cfg.mouse.profiles[0].button_mappings_enabled = None;
        // Device profile: G3 V2 turns it off.
        let mut dev = crate::config::MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(10007),
                product_id: Some(12976),
                ..Default::default()
            },
            ..Default::default()
        };
        dev.button_mappings_enabled = Some(false);
        cfg.mouse.profiles.push(dev);
        // All-Mice resolve: true.
        assert!(resolve_from(&cfg, None).button_mappings_enabled);
        // G3 V2 -> false.
        assert!(!resolve_from(&cfg, Some((10007, 12976))).button_mappings_enabled);
        // Another device -> true.
        assert!(resolve_from(&cfg, Some((1, 2))).button_mappings_enabled);
    }

    use crate::config::PartialPointerSection;

    #[test]
    fn wildcard_only_falls_back_to_defaults() {
        let mut cfg = Config::default();
        // The default config has one "all mice" profile holding the defaults.
        let r = resolve_from(&cfg, None);
        assert!(!r.reverse_scroll);
        let r2 = resolve_from(&cfg, Some((1133, 17492)));
        assert!(!r2.reverse_scroll);
        let _ = &mut cfg;
    }

    #[test]
    fn per_device_overrides_wildcard() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // "All mice" profile: reverse scrolling on.
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        // Device profile: reverse scrolling off.
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(17492),
                ..Default::default()
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });

        // Matching device: the later profile wins -> off.
        let r = resolve_from(&cfg, Some((1133, 17492)));
        assert!(!r.reverse_scroll);
        // Other device: only the wildcard profile applies -> on.
        let r2 = resolve_from(&cfg, Some((1, 2)));
        assert!(r2.reverse_scroll);
        // No device (fallback): only the wildcard profile applies -> on.
        let r3 = resolve_from(&cfg, None);
        assert!(r3.reverse_scroll);
    }

    #[test]
    fn acceleration_merges_across_profiles() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // "All mice" profile: set pointer acceleration.
        cfg.mouse.profiles.push(MouseProfile {
            pointer: Some(PartialPointerSection {
                acceleration: Some(0.6875),
                ..Default::default()
            }),
            ..Default::default()
        });
        // Device profile: override with a different value.
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(17492),
                ..Default::default()
            },
            pointer: Some(PartialPointerSection {
                acceleration: Some(2.0),
                ..Default::default()
            }),
            ..Default::default()
        });

        // Matching device: the device profile wins.
        assert_eq!(
            resolve_from(&cfg, Some((1133, 17492))).acceleration,
            Some(2.0)
        );
        // Other device: the wildcard profile applies.
        assert_eq!(resolve_from(&cfg, Some((1, 2))).acceleration, Some(0.6875));
        // No device (attribution failed): only the wildcard profile applies.
        assert_eq!(resolve_from(&cfg, None).acceleration, Some(0.6875));

        // No profile sets it -> None (leave the device's current value alone).
        let mut cfg2 = Config::default();
        cfg2.mouse.profiles.clear();
        cfg2.mouse.profiles.push(MouseProfile::default());
        assert_eq!(resolve_from(&cfg2, None).acceleration, None);
    }

    #[test]
    fn later_match_wins_on_merge() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // Two wildcard profiles: the later one overrides the earlier.
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            line_count: Some(5),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(false),
            ..Default::default()
        });
        let r = resolve_from(&cfg, None);
        assert!(!r.reverse_scroll); // the later profile wins
        assert_eq!(r.line_count, 5); // the earlier field is kept (the later profile does not set it)
        let _ = &mut cfg;
    }

    #[test]
    fn virtual_profile_matches_only_injected_events() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // "All Mice" layer: reverse scrolling on (the virtual pointer's base layer).
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        // Virtual-pointer profile: reverse off (overriding the base layer) plus one button mapping.
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                injected: Some(true),
                ..Default::default()
            },
            reverse_scroll: Some(false),
            button_mappings: [("3".to_string(), "switcher".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        });

        // Injected event -> the virtual profile wins over "All Mice", mapping included.
        let r = resolve_from(&cfg, Some(crate::mouse::device::VIRTUAL_DEVICE_KEY));
        assert!(!r.reverse_scroll);
        assert_eq!(
            r.button_mappings.get("3").map(String::as_str),
            Some("switcher")
        );

        // A real device -> the virtual profile stays out; only "All Mice" applies.
        let r = resolve_from(&cfg, Some((10007, 12976)));
        assert!(r.reverse_scroll);
        assert!(r.button_mappings.is_empty());
    }

    #[test]
    fn virtual_profile_survives_toml_roundtrip() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                injected: Some(true),
                ..Default::default()
            },
            reverse_scroll: Some(true),
            ..Default::default()
        });
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        // The flag persists as a flat key (same convention as device_vendor_id / product_id).
        assert!(toml_str.contains("device_injected = true"));
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        let p = parsed.mouse.profiles.last().unwrap();
        assert!(p.device.is_virtual());
        assert!(p.device.vendor_id.is_none());
    }

    #[test]
    fn toml_roundtrip_preserves_profiles() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            scroll_mode: Some("line".into()),
            line_count: Some(7),
            pointer: Some(PartialPointerSection {
                disable_acceleration: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0xC548),
                product_id: Some(0x4444),
                ..Default::default()
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        // After deserialization, both profiles should survive.
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles.len(), 2);
        assert!(parsed.mouse.profiles[0].device.vendor_id.is_none());
        assert_eq!(parsed.mouse.profiles[1].device.vendor_id, Some(0xC548));
        assert_eq!(parsed.mouse.profiles[1].reverse_scroll, Some(false));
        // Resolution should match the original.
        let r = resolve_from(&parsed, Some((0xC548, 0x4444)));
        assert!(!r.reverse_scroll); // the device profile overrides the wildcard one
        assert_eq!(r.line_count, 7); // comes from the wildcard profile (line_count)
        assert!(r.disable_acceleration); // comes from the wildcard profile
    }

    #[test]
    fn legacy_flat_fields_migrate_to_profile() {
        let toml_str = r#"
[mouse]
enabled = true
reverse_scroll = true
scroll_mode = "line"
line_count = 5
[mouse.pointer]
disable_acceleration = true
"#;
        let mut parsed: Config = toml::from_str(toml_str).unwrap();
        parsed.mouse.migrate_legacy();
        // After migration there should be one "All Mice" profile carrying the legacy values.
        assert_eq!(parsed.mouse.profiles.len(), 1);
        let p = &parsed.mouse.profiles[0];
        assert_eq!(p.reverse_scroll, Some(true));
        assert_eq!(p.scroll_mode.as_deref(), Some("line"));
        assert_eq!(p.line_count, Some(5));
        assert_eq!(
            p.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        // Legacy fields should be cleared (avoid serializing cruft).
        assert!(parsed.mouse.reverse_scroll.is_none());
        assert!(parsed.mouse.scroll_mode.is_none());
    }
}
