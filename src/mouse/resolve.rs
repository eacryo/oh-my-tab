//! 配置解析引擎:把"所有鼠标"档 + per-device 档合并成具体生效配置(Phase 3)。
//!
//! 合并语义:遍历 CONFIG.mouse.profiles,对每个匹配的档(无 device = 通配,有 device =
//! VID+PID 相等)把 Some 字段并入结果,后者优先。"所有鼠标"档通常在前,per-device 档在后。
//!
//! Config resolution: merge the "All Mice" profile + per-device profiles into the effective
//! config (Phase 3). Merge semantics: iterate CONFIG.mouse.profiles; for each matching profile
//! (no device = wildcard, device = VID+PID equality) fold its Some fields into the result,
//! later profiles winning. The "All Mice" profile usually comes first, per-device ones after.

use crate::config::{Config, MouseProfile, CONFIG};
use crate::mouse::device::DeviceKey;
use crate::mouse::scrolling::ScrollMode;
use std::sync::Mutex;

/// 解析后的具体生效配置(非 Option,所有字段已定)。
/// Resolved effective config (non-Option; all fields are concrete).
#[derive(Debug, Clone)]
pub(crate) struct ResolvedMouse {
    pub reverse_scroll: bool,
    pub scroll_mode: ScrollMode,
    pub line_count: u32,
    pub disable_acceleration: bool,
}

impl Default for ResolvedMouse {
    fn default() -> Self {
        Self {
            reverse_scroll: false,
            scroll_mode: ScrollMode::Default,
            line_count: 3,
            disable_acceleration: false,
        }
    }
}

/// 解析缓存:key = (VID,PID),None 键 = "无设备/所有鼠标"。在 reload_config / 设备变更时失效。
/// Resolve cache: key = (VID, PID); the None key = "no device / All Mice". Invalidated on
/// reload_config and device changes.
static CACHE: std::sync::LazyLock<
    Mutex<std::collections::HashMap<Option<DeviceKey>, ResolvedMouse>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// 使缓存失效(配置重载或设备变更时调用)。
/// Invalidate the cache (called on config reload or device changes).
pub(crate) fn invalidate_cache() {
    if let Ok(mut c) = CACHE.lock() {
        c.clear();
    }
}

/// 匹配器是否匹配给定设备。None 键 = 通配(匹配所有设备,即"所有鼠标"档)。
/// Whether a matcher matches the given device. A None key = wildcard (matches all devices,
/// i.e. the "All Mice" profile).
fn matches(profile: &MouseProfile, device: Option<DeviceKey>) -> bool {
    let Some((vid, pid)) = device else {
        // 无设备(归因失败回退):只匹配通配档。
        // No device (attribution-failure fallback): match only wildcard profiles.
        return profile.device.vendor_id.is_none() && profile.device.product_id.is_none();
    };
    let vid_ok = profile.device.vendor_id.map(|v| v == vid).unwrap_or(true);
    let pid_ok = profile.device.product_id.map(|p| p == pid).unwrap_or(true);
    vid_ok && pid_ok
}

/// 解析某设备的生效配置。device = None 表示归因失败,只用"所有鼠标"档。
/// Resolve the effective config for a device. device = None means attribution failed; only the
/// "All Mice" profile applies.
pub(crate) fn resolve(device: Option<DeviceKey>) -> ResolvedMouse {
    // 查缓存。
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

/// 从给定 Config 解析某设备的生效配置(供设置预览等非 CONFIG 场景)。
/// Resolve a device's effective config from a given Config (for non-CONFIG contexts like the
/// restore-defaults preview).
pub(crate) fn resolve_from_config(
    cfg: &crate::config::Config,
    device: Option<DeviceKey>,
) -> ResolvedMouse {
    resolve_from(cfg, device)
}

/// 从给定 Config 解析(供测试与无 CONFIG 的场景)。
/// Resolve from a given Config (for tests and CONFIG-free scenarios).
fn resolve_from(cfg: &Config, device: Option<DeviceKey>) -> ResolvedMouse {
    let mut r = ResolvedMouse::default();

    // 先用代码默认值兜底(确保所有字段有值)。
    // Start from code defaults so every field is concrete.
    let defaults = ResolvedMouse::default();
    r.reverse_scroll = defaults.reverse_scroll;
    r.scroll_mode = defaults.scroll_mode;
    r.line_count = defaults.line_count;
    r.disable_acceleration = defaults.disable_acceleration;

    // 遍历 profiles,合并所有匹配档(后者优先)。
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
        }
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PartialPointerSection;

    #[test]
    fn wildcard_only_falls_back_to_defaults() {
        let mut cfg = Config::default();
        // 默认配置含一个"所有鼠标"档,值为默认。
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
        // "所有鼠标"档:反转滚动开。
        cfg.mouse.profiles.push(MouseProfile {
            reverse_scroll: Some(true),
            ..Default::default()
        });
        // 某设备档:反转滚动关。
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(1133),
                product_id: Some(17492),
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });

        // 匹配的设备:后者优先 -> 关。
        let r = resolve_from(&cfg, Some((1133, 17492)));
        assert!(!r.reverse_scroll);
        // 其他设备:只用通配档 -> 开。
        let r2 = resolve_from(&cfg, Some((1, 2)));
        assert!(r2.reverse_scroll);
        // 无设备(回退):只用通配档 -> 开。
        let r3 = resolve_from(&cfg, None);
        assert!(r3.reverse_scroll);
    }

    #[test]
    fn later_match_wins_on_merge() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        // 两条通配档:后者覆盖前者。
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
        assert!(!r.reverse_scroll); // 后者胜
        assert_eq!(r.line_count, 5); // 前者字段保留(后者未设)
        let _ = &mut cfg;
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
            }),
            ..Default::default()
        });
        cfg.mouse.profiles.push(MouseProfile {
            device: crate::config::DeviceMatcher {
                vendor_id: Some(0xC548),
                product_id: Some(0x4444),
            },
            reverse_scroll: Some(false),
            ..Default::default()
        });
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        // 反序列化后应保留两条档。
        // After deserialization, both profiles should survive.
        let parsed: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.mouse.profiles.len(), 2);
        assert!(parsed.mouse.profiles[0].device.vendor_id.is_none());
        assert_eq!(parsed.mouse.profiles[1].device.vendor_id, Some(0xC548));
        assert_eq!(parsed.mouse.profiles[1].reverse_scroll, Some(false));
        // 解析结果应与原配置一致。
        // Resolution should match the original.
        let r = resolve_from(&parsed, Some((0xC548, 0x4444)));
        assert!(!r.reverse_scroll); // 设备档覆盖通配档
        assert_eq!(r.line_count, 7); // 来自通配档(line_count)
        assert!(r.disable_acceleration); // 来自通配档
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
        // 迁移后应有一个"所有鼠标"档,字段从旧值搬入。
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
        // 旧字段应被清空(防止序列化出冗余)。
        // Legacy fields should be cleared (avoid serializing cruft).
        assert!(parsed.mouse.reverse_scroll.is_none());
        assert!(parsed.mouse.scroll_mode.is_none());
    }
}
