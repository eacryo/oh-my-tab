use serde::{Deserialize, Serialize};
use std::sync::RwLock;

use crate::i18n::{self, tf};

// ========== Structs ==========

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub appearance: Appearance,
    pub layout: Layout,
    pub colors: ColorsSection,
    pub fonts: Fonts,
    pub keyboard: Keyboard,
    pub i18n: I18nSection,
    pub windows: WindowsSection,
    pub logging: LoggingSection,
    pub startup: StartupSection,
    pub clipboard: ClipboardSection,
    pub mouse: MouseSection,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub glass_style: String,
    pub glass_tint: String,
    pub corner_radius: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Layout {
    pub cards_per_row: usize,
    pub card_width: f64,
    pub card_height: f64,
    pub card_gap: f64,
    pub icon_size: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ColorsSection {
    pub dark: ThemeColors,
    pub light: ThemeColors,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ThemeColors {
    pub status_bar_text: String,
    pub app_name: String,
    pub win_title: String,
    pub icon_inner_bg: String,
    pub icon_text: String,
    pub card_bg_sel: String,
    pub card_border_sel: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Fonts {
    pub status_bar_size: f64,
    pub status_bar_weight: f64,
    pub title_size: f64,
    pub title_weight: f64,
    pub app_name_size: f64,
    pub app_name_weight: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Keyboard {
    pub modifier: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct I18nSection {
    pub locale: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WindowsSection {
    // 默认 false(不显示最小化窗口,与历史行为一致);bool::default() 即 false,故 Default 可直接派生。
    // Defaults to false (hide minimized windows, matching prior behavior); bool::default() is
    // false, so Default can be derived directly.
    pub show_minimized: bool,
    // 浮窗显示位置:"active_window" = 跟随激活窗口所在屏幕,"main" = 始终显示在主屏幕。
    // 默认跟随激活窗口(多显示器用户开箱即得新体验)。
    // Overlay display position: "active_window" = follow the active window's screen,
    // "main" = always on the main screen. Defaults to following the active window.
    pub overlay_position: String,
}

impl Default for WindowsSection {
    fn default() -> Self {
        Self {
            show_minimized: false,
            overlay_position: "active_window".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingSection {
    // 日志级别:"debug","info";默认 "info"(常规档,不刷屏;debug 输出全量调试细节)。
    // Log level: "debug" | "info"; default "info" (normal tier, no spam; debug emits all detail).
    pub level: String,
    // 日志文件路径;空=使用默认路径 ~/Library/Logs/oh-my-tab/oh-my-tab-<时间戳>.log。
    // Log file path; empty = use the default (timestamped file under ~/Library/Logs/oh-my-tab/).
    pub file_path: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct StartupSection {
    // 开机自启;默认 false,bool::default() 即 false,故 Default 可直接派生。
    // Launch at login; defaults to false (bool::default() is false, so Default derives directly).
    pub launch_at_login: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ClipboardSection {
    // 历史剪贴板总开关;默认 false(不启动剪贴板轮询)。
    // History-clipboard master switch; defaults to false (no pasteboard polling).
    pub enabled: bool,
    // 历史最大条数(1..=100,默认 50)。第一版仅内存,不持久化。
    // Max history entries (1..=100, default 50). v1 is in-memory only, no persistence.
    pub max_entries: u32,
    // 显示来源应用:复制时始终记录来源(ClipEntry.source_app),此开关只控制是否在
    // 条目里显示应用名。默认 false。
    // Show the source app: the source is ALWAYS recorded at copy time (ClipEntry.source_app);
    // this switch only controls whether the row displays the app name. Default false.
    pub show_source_app: bool,
}

impl Default for ClipboardSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_entries: 50,
            show_source_app: false,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PointerSection {
    // 禁用系统鼠标加速,光标 1:1 线性跟踪。默认 false。
    // Disable system pointer acceleration for 1:1 linear cursor tracking. Default false.
    pub disable_acceleration: bool,
}

/// 设备匹配器(None = 通配,即"所有鼠标")。配置按 VID+PID 匹配设备。
/// Device matcher (None = wildcard, i.e. "All Mice"). Config matches devices by VID+PID.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DeviceMatcher {
    // 扁平序列化为 device_vendor_id / device_product_id(顶层标量,便于手写 TOML)。
    // Flattened as device_vendor_id / device_product_id (top-level scalars for hand-written TOML).
    #[serde(rename = "device_vendor_id", skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u32>,
    #[serde(rename = "device_product_id", skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u32>,
}

/// 指针覆盖(部分字段,None = 继承下层档)。
/// Pointer override (partial; None = inherit from the lower layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PartialPointerSection {
    pub disable_acceleration: Option<bool>,
}

/// 单个配置档。device = None 即"所有鼠标"档(默认层)。
/// A single profile. device = None is the "All Mice" profile (the default layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct MouseProfile {
    /// 设备匹配器;None = 匹配所有鼠标(作为默认层)。
    /// Device matcher; None = matches all mice (serves as the default layer).
    #[serde(flatten)]
    pub device: DeviceMatcher,
    // 反转滚动方向。true = 相对系统当前方向取反(与 LinearMouse 一致,不读自然滚动设置:
    // HID tap 事件已含系统自然滚动翻转,合成事件不再被翻转,见 scrolling.rs 的 should_flip)。
    // Reverse scroll direction. true = flip relative to the system's current direction (same as
    // LinearMouse; no natural-scroll setting is read: HID-tap events already carry the system
    // natural-scroll flip and synthetic events aren't flipped again, see should_flip in scrolling.rs).
    pub reverse_scroll: Option<bool>,
    pub scroll_mode: Option<String>,
    // Line 模式每格行数(1..=10)。
    // Line mode lines per notch (1..=10).
    pub line_count: Option<u32>,
    pub pointer: Option<PartialPointerSection>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct MouseSection {
    // 启用鼠标控制功能(总开关)。默认 false(不启用,不创建 event tap)。
    // Enable mouse control features (master switch). Default false.
    pub enabled: bool,
    // 配置档列表:第一个无 device 字段的档是"所有鼠标"默认层;其后可加 per-device 档,
    // 后者覆盖前者。合并语义:遍历所有匹配档,后者优先。
    // Profile list: the first profile without a device field is the "All Mice" default layer;
    // subsequent per-device profiles override it. Merge semantics: all matching profiles are
    // traversed, later ones win.
    pub profiles: Vec<MouseProfile>,

    // ---- 旧字段(仅用于迁移读取,序列化时跳过)----
    // 用 Option 承载:旧版本配置文件显式写出的扁平字段反序列化后才有 Some 值;
    // 新版本序列化跳过这些字段,重载后必为 None —— 从根上杜绝"serde 兜底值误判
    // 含旧字段"导致的覆盖 bug(见 docs/test-review.md)。
    // ---- Legacy fields (read for migration only, skipped on serialize) ----
    // Option-typed on purpose: only old-format files that explicitly wrote these flat keys
    // deserialize to Some; new-format files skip them on serialize, so they always reload as
    // None -- eliminating the "serde-default masquerades as legacy content" overwrite bug
    // (see docs/test-review.md).
    #[serde(skip_serializing)]
    pub reverse_scroll: Option<bool>,
    #[serde(skip_serializing)]
    pub scroll_mode: Option<String>,
    #[serde(skip_serializing)]
    pub line_count: Option<u32>,
    #[serde(skip_serializing)]
    pub pointer: Option<PointerSection>,
}

impl Default for MouseSection {
    fn default() -> Self {
        Self {
            enabled: false,
            // 默认含一个"所有鼠标"档,与旧默认值一致(reverse_scroll=false, default 模式, 3 行)。
            // Default includes an "All Mice" profile matching the old defaults.
            profiles: vec![MouseProfile {
                reverse_scroll: Some(false),
                scroll_mode: Some("default".into()),
                line_count: Some(3),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(false),
                }),
                ..Default::default()
            }],
            reverse_scroll: None,
            scroll_mode: None,
            line_count: None,
            pointer: None,
        }
    }
}

impl MouseSection {
    /// 若旧字段被填充(旧版本配置文件写出的扁平字段),把旧字段迁移成一个"所有鼠标"档。
    /// 迁移是幂等的:已迁移的配置(无旧字段、有默认档)不会重复迁移。
    /// 返回是否有改动(迁移了旧字段,或补插了默认档)——调用方据此决定是否写回磁盘。
    ///
    /// Migrate legacy flat fields into an "All Mice" profile. Idempotent: an already-migrated
    /// config (no legacy fields, has a default profile) is left untouched. Returns whether
    /// anything changed (legacy fields migrated, or a default profile inserted) -- the caller
    /// uses it to decide whether to rewrite the file.
    pub(crate) fn migrate_legacy(&mut self) -> bool {
        // 旧字段是 Option:只有旧版本文件显式写出的键才有 Some 值。新版本序列化跳过
        // 这些字段,重载后必为 None,不会因 serde 兜底值(曾填 "default")误判含旧字段
        // 而覆盖用户在新格式 profiles 里的配置。
        // Legacy fields are Option-typed: only keys explicitly written by old-format files are
        // Some. New-format files skip them on serialize, so they reload as None and no longer
        // trigger a migration that would clobber user profiles (serde used to backfill
        // "default" into the legacy scroll_mode, making has_legacy always true).
        let has_legacy = self.reverse_scroll.is_some()
            || self.scroll_mode.is_some()
            || self.line_count.is_some()
            || self.pointer.is_some();

        if !has_legacy {
            // 无旧字段(全新配置或已迁移):确保至少有一个默认"所有鼠标"档。
            // No legacy content (fresh or already migrated): ensure a default "All Mice" profile exists.
            let has_default = self
                .profiles
                .iter()
                .any(|p| p.device.vendor_id.is_none() && p.device.product_id.is_none());
            if !has_default {
                self.profiles.insert(0, Self::default().profiles[0].clone());
                return true;
            }
            return false;
        }

        // 有旧字段:把它们并入(或新建)一个"所有鼠标"档。
        // Legacy fields present: fold them into (or create) an "All Mice" profile.
        let legacy_profile = MouseProfile {
            reverse_scroll: self.reverse_scroll,
            scroll_mode: self.scroll_mode.clone().map(|m| {
                if m.is_empty() {
                    "default".into()
                } else {
                    m
                }
            }),
            line_count: self.line_count.map(|n| if n == 0 { 3 } else { n }),
            pointer: self.pointer.take().map(|p| PartialPointerSection {
                disable_acceleration: Some(p.disable_acceleration),
            }),
            ..Default::default()
        };

        // 若已有"所有鼠标"档,用旧字段值覆盖其字段(旧字段是用户真实意图)。
        // If an "All Mice" profile already exists, overwrite its fields with the legacy values
        // (the legacy fields are the user's true intent).
        if let Some(idx) = self
            .profiles
            .iter()
            .position(|p| p.device.vendor_id.is_none() && p.device.product_id.is_none())
        {
            self.profiles[idx] = legacy_profile;
        } else {
            // 无"所有鼠标"档:在队首插入(确保默认档在前,per-device 档在后)。
            // No "All Mice" profile: insert at the front (default first, per-device after).
            self.profiles.insert(0, legacy_profile);
        }

        // 清掉旧字段(防止下次再迁移;序列化本就跳过它们)。
        // Clear legacy fields (prevents re-migration; serialization skips them anyway).
        self.reverse_scroll = None;
        self.scroll_mode = None;
        self.line_count = None;
        self.pointer = None;
        true
    }
}

// ========== Default implementations (hard-coded fallback values) ==========

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: "light".into(),
            glass_style: "regular".into(),
            glass_tint: "eeeeee66".into(),
            corner_radius: 64.0,
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            cards_per_row: 6,
            card_width: 140.0,
            card_height: 180.0,
            card_gap: 0.0,
            icon_size: 110.0,
        }
    }
}

impl Default for ColorsSection {
    fn default() -> Self {
        ColorsSection {
            dark: ThemeColors::dark_default(),
            light: ThemeColors::light_default(),
        }
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self::dark_default()
    }
}

impl ThemeColors {
    fn dark_default() -> Self {
        ThemeColors {
            status_bar_text: "999999ff".into(),
            app_name: "ddddddff".into(),
            win_title: "888888ff".into(),
            icon_inner_bg: "22224444".into(),
            icon_text: "9999bbff".into(),
            card_bg_sel: "22224444".into(),
            card_border_sel: "5577ccff".into(),
        }
    }

    fn light_default() -> Self {
        ThemeColors {
            status_bar_text: "333333ff".into(),
            app_name: "1a1a1aff".into(),
            win_title: "333333ff".into(),
            icon_inner_bg: "d0d0e066".into(),
            icon_text: "666688ff".into(),
            card_bg_sel: "ffffff66".into(),
            card_border_sel: "5577ccff".into(),
        }
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Fonts {
            status_bar_size: 13.0,
            status_bar_weight: 0.23,
            title_size: 11.0,
            title_weight: 0.23,
            app_name_size: 13.0,
            app_name_weight: 0.5,
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Keyboard {
            // 默认 Cmd+Tab;用户可在设置里切回 Option+Tab。
            // Default Cmd+Tab; users can switch back to Option+Tab in Settings.
            modifier: "command".into(),
        }
    }
}

impl Default for I18nSection {
    fn default() -> Self {
        I18nSection {
            locale: "auto".into(), // 跟随系统语言 / follow system language
        }
    }
}

impl Default for LoggingSection {
    fn default() -> Self {
        LoggingSection {
            level: "info".into(),
            file_path: String::new(),
        }
    }
}

// ========== Validation ==========

fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl Config {
    pub fn validate(&self) -> Vec<String> {
        let mut errs: Vec<String> = Vec::new();

        // --- appearance ---
        if !["dark", "light", "auto"].contains(&self.appearance.theme.as_str()) {
            errs.push(tf(
                "errors.appearance_theme_invalid",
                &[("value", &self.appearance.theme)],
            ));
        }
        if !["regular", "clear"].contains(&self.appearance.glass_style.as_str()) {
            errs.push(tf(
                "errors.appearance_glass_style_invalid",
                &[("value", &self.appearance.glass_style)],
            ));
        }
        if !is_hex8(&self.appearance.glass_tint) {
            errs.push(tf(
                "errors.appearance_glass_tint_invalid",
                &[("value", &self.appearance.glass_tint)],
            ));
        }
        if self.appearance.corner_radius < 0.0 {
            errs.push(tf(
                "errors.appearance_corner_radius_invalid",
                &[("value", &self.appearance.corner_radius.to_string())],
            ));
        }

        // --- layout ---
        if self.layout.cards_per_row < 1 || self.layout.cards_per_row > 10 {
            errs.push(tf(
                "errors.layout_cards_per_row_invalid",
                &[("value", &self.layout.cards_per_row.to_string())],
            ));
        }
        if self.layout.card_width < 80.0 {
            errs.push(tf(
                "errors.layout_card_width_invalid",
                &[("value", &self.layout.card_width.to_string())],
            ));
        }
        if self.layout.card_height < 100.0 {
            errs.push(tf(
                "errors.layout_card_height_invalid",
                &[("value", &self.layout.card_height.to_string())],
            ));
        }
        if self.layout.card_gap < 0.0 {
            errs.push(tf(
                "errors.layout_card_gap_invalid",
                &[("value", &self.layout.card_gap.to_string())],
            ));
        }
        if self.layout.icon_size < 32.0 {
            errs.push(tf(
                "errors.layout_icon_size_invalid",
                &[("value", &self.layout.icon_size.to_string())],
            ));
        }

        // --- colors ---
        for (theme, colors) in [("dark", &self.colors.dark), ("light", &self.colors.light)] {
            let prefix = format!("colors.{theme}");
            if !is_hex8(&colors.status_bar_text) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.status_bar_text"))],
                ));
            }
            if !is_hex8(&colors.app_name) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.app_name"))],
                ));
            }
            if !is_hex8(&colors.win_title) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.win_title"))],
                ));
            }
            if !is_hex8(&colors.icon_inner_bg) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.icon_inner_bg"))],
                ));
            }
            if !is_hex8(&colors.icon_text) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.icon_text"))],
                ));
            }
            if !is_hex8(&colors.card_bg_sel) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.card_bg_sel"))],
                ));
            }
            if !is_hex8(&colors.card_border_sel) {
                errs.push(tf(
                    "errors.colors_not_hex8",
                    &[("field", &format!("{prefix}.card_border_sel"))],
                ));
            }
        }

        // --- fonts ---
        if self.fonts.status_bar_size < 8.0 {
            errs.push(tf(
                "errors.fonts_size_invalid",
                &[
                    ("field", "fonts.status_bar_size"),
                    ("value", &self.fonts.status_bar_size.to_string()),
                ],
            ));
        }
        if self.fonts.status_bar_weight < 0.0 || self.fonts.status_bar_weight > 1.0 {
            errs.push(tf(
                "errors.fonts_weight_invalid",
                &[
                    ("field", "fonts.status_bar_weight"),
                    ("value", &self.fonts.status_bar_weight.to_string()),
                ],
            ));
        }
        if self.fonts.title_size < 8.0 {
            errs.push(tf(
                "errors.fonts_size_invalid",
                &[
                    ("field", "fonts.title_size"),
                    ("value", &self.fonts.title_size.to_string()),
                ],
            ));
        }
        if self.fonts.title_weight < 0.0 || self.fonts.title_weight > 1.0 {
            errs.push(tf(
                "errors.fonts_weight_invalid",
                &[
                    ("field", "fonts.title_weight"),
                    ("value", &self.fonts.title_weight.to_string()),
                ],
            ));
        }
        if self.fonts.app_name_size < 8.0 {
            errs.push(tf(
                "errors.fonts_size_invalid",
                &[
                    ("field", "fonts.app_name_size"),
                    ("value", &self.fonts.app_name_size.to_string()),
                ],
            ));
        }
        if self.fonts.app_name_weight < 0.0 || self.fonts.app_name_weight > 1.0 {
            errs.push(tf(
                "errors.fonts_weight_invalid",
                &[
                    ("field", "fonts.app_name_weight"),
                    ("value", &self.fonts.app_name_weight.to_string()),
                ],
            ));
        }

        // --- keyboard ---
        if !["option", "command"].contains(&self.keyboard.modifier.as_str()) {
            errs.push(tf(
                "errors.keyboard_modifier_invalid",
                &[("value", &self.keyboard.modifier)],
            ));
        }

        // --- i18n ---
        if !["auto", "en", "zh-Hans", "zh-Hant"].contains(&self.i18n.locale.as_str()) {
            errs.push(tf(
                "errors.i18n_locale_invalid",
                &[("value", &self.i18n.locale)],
            ));
        }

        // --- logging ---
        if !["debug", "info"].contains(&self.logging.level.as_str()) {
            errs.push(tf(
                "errors.logging_level_invalid",
                &[("value", &self.logging.level)],
            ));
        }

        // --- windows ---
        if !["active_window", "main"].contains(&self.windows.overlay_position.as_str()) {
            errs.push(tf(
                "errors.windows_overlay_position_invalid",
                &[("value", &self.windows.overlay_position)],
            ));
        }

        // --- clipboard ---
        if !(1..=100).contains(&self.clipboard.max_entries) {
            errs.push(tf(
                "errors.clipboard_max_entries_invalid",
                &[("value", &self.clipboard.max_entries.to_string())],
            ));
        }

        // --- mouse profiles ---
        for (i, p) in self.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            if let Some(ref mode) = p.scroll_mode {
                if !["default", "line"].contains(&mode.as_str()) {
                    errs.push(tf("errors.mouse_scroll_mode_invalid", &[("value", mode)]));
                    // 用 prefix 区分哪个档出错,便于定位。
                    // Use the prefix to indicate which profile failed.
                    if let Some(last) = errs.last_mut() {
                        *last = format!("{prefix}.scroll_mode: {last}");
                    }
                }
            }
            if let Some(lc) = p.line_count {
                if !(1..=10).contains(&lc) {
                    let msg = tf(
                        "errors.mouse_line_count_invalid",
                        &[("value", &lc.to_string())],
                    );
                    errs.push(format!("{prefix}.line_count: {msg}"));
                }
            }
        }

        errs
    }

    /// Merge valid fields from `other` into `self`, keeping defaults for invalid fields.
    /// Returns the list of fields that were rejected (with reasons).
    pub fn merge_valid(&mut self, other: Config, errs: &[String]) {
        // For each top-level section, if validation had no errors for that section,
        // keep the loaded value; otherwise the Default (already in `self`) stays.
        //
        // Simpler approach: use `other` wholesale but reset individual fields that
        // had errors back to the defaults.
        let has_error = |prefix: &str| errs.iter().any(|e| e.starts_with(prefix));

        // appearance
        if !has_error("appearance") {
            self.appearance = other.appearance;
        } else {
            // Selective merge: only keep valid sub-fields
            if !errs.iter().any(|e| e.starts_with("appearance.theme")) {
                self.appearance.theme = other.appearance.theme;
            }
            if !errs.iter().any(|e| e.starts_with("appearance.glass_style")) {
                self.appearance.glass_style = other.appearance.glass_style;
            }
            if !errs.iter().any(|e| e.starts_with("appearance.glass_tint")) {
                self.appearance.glass_tint = other.appearance.glass_tint;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("appearance.corner_radius"))
            {
                self.appearance.corner_radius = other.appearance.corner_radius;
            }
        }

        // layout
        if !has_error("layout.") {
            self.layout = other.layout;
        } else {
            if !errs.iter().any(|e| e.starts_with("layout.cards_per_row")) {
                self.layout.cards_per_row = other.layout.cards_per_row;
            }
            if !errs.iter().any(|e| e.starts_with("layout.card_width")) {
                self.layout.card_width = other.layout.card_width;
            }
            if !errs.iter().any(|e| e.starts_with("layout.card_height")) {
                self.layout.card_height = other.layout.card_height;
            }
            if !errs.iter().any(|e| e.starts_with("layout.card_gap")) {
                self.layout.card_gap = other.layout.card_gap;
            }
            if !errs.iter().any(|e| e.starts_with("layout.icon_size")) {
                self.layout.icon_size = other.layout.icon_size;
            }
        }

        // colors
        if !has_error("colors.") {
            self.colors = other.colors;
        } else {
            // Per-theme, per-field merge
            for (theme, ours, theirs) in [
                ("dark", &mut self.colors.dark, &other.colors.dark),
                ("light", &mut self.colors.light, &other.colors.light),
            ] {
                Self::merge_colors(ours, theirs, theme, errs);
            }
        }

        // fonts
        if !has_error("fonts.") {
            self.fonts = other.fonts;
        } else {
            if !errs.iter().any(|e| e.starts_with("fonts.status_bar_size")) {
                self.fonts.status_bar_size = other.fonts.status_bar_size;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("fonts.status_bar_weight"))
            {
                self.fonts.status_bar_weight = other.fonts.status_bar_weight;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.title_size")) {
                self.fonts.title_size = other.fonts.title_size;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.title_weight")) {
                self.fonts.title_weight = other.fonts.title_weight;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.app_name_size")) {
                self.fonts.app_name_size = other.fonts.app_name_size;
            }
            if !errs.iter().any(|e| e.starts_with("fonts.app_name_weight")) {
                self.fonts.app_name_weight = other.fonts.app_name_weight;
            }
        }

        // keyboard
        if !has_error("keyboard.") {
            self.keyboard = other.keyboard;
        } else {
            if !errs.iter().any(|e| e.starts_with("keyboard.modifier")) {
                self.keyboard.modifier = other.keyboard.modifier;
            }
        }

        // i18n
        if !has_error("i18n.") {
            self.i18n = other.i18n;
        } else {
            if !errs.iter().any(|e| e.starts_with("i18n.locale")) {
                self.i18n.locale = other.i18n.locale;
            }
        }

        // windows
        if !has_error("windows.") {
            self.windows = other.windows;
        } else {
            if !errs.iter().any(|e| e.starts_with("windows.show_minimized")) {
                self.windows.show_minimized = other.windows.show_minimized;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("windows.overlay_position"))
            {
                self.windows.overlay_position = other.windows.overlay_position;
            }
        }

        // logging
        if !has_error("logging.") {
            self.logging = other.logging;
        } else {
            if !errs.iter().any(|e| e.starts_with("logging.level")) {
                self.logging.level = other.logging.level;
            }
            // file_path 无校验,恒有效 / file_path has no validation, always valid
            self.logging.file_path = other.logging.file_path;
        }

        // startup (bool 字段无需校验,恒有效)
        // startup (bool field needs no validation, always valid)
        self.startup = other.startup;

        // clipboard (enabled 恒有效;max_entries 有校验)
        // clipboard (enabled always valid; max_entries is validated)
        self.clipboard.enabled = other.clipboard.enabled;
        // show_source_app 是布尔,恒有效。
        // show_source_app is a bool, always valid.
        self.clipboard.show_source_app = other.clipboard.show_source_app;
        if !errs.iter().any(|e| e.starts_with("clipboard.max_entries")) {
            self.clipboard.max_entries = other.clipboard.max_entries;
        }

        // mouse:profiles 逐档逐字段合并(沿用 per-field resilient 模式)。
        // enabled 与 bool 字段恒有效;profiles 的每个档按字段校验结果保留或丢弃。
        // mouse: per-profile, per-field merge (continuing the per-field resilient pattern).
        // enabled and bool fields are always valid; each profile's fields are kept or dropped
        // based on per-field validation results.
        self.mouse.enabled = other.mouse.enabled;
        // 先迁移 other 的旧字段(若有),再合并。
        // Migrate other's legacy fields (if any) before merging.
        let mut other_mouse = other.mouse;
        other_mouse.migrate_legacy();
        self.mouse.profiles = Vec::new();
        for (i, p) in other_mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            let mut merged_p = MouseProfile {
                device: p.device.clone(),
                ..Default::default()
            };
            // bool 字段恒有效。
            // Bool fields are always valid.
            merged_p.reverse_scroll = p.reverse_scroll;
            if p.scroll_mode.is_some()
                && !errs
                    .iter()
                    .any(|e| e.starts_with(&format!("{prefix}.scroll_mode")))
            {
                merged_p.scroll_mode = p.scroll_mode.clone();
            }
            if p.line_count.is_some()
                && !errs
                    .iter()
                    .any(|e| e.starts_with(&format!("{prefix}.line_count")))
            {
                merged_p.line_count = p.line_count;
            }
            // pointer.disable_acceleration 是 bool,恒有效。
            // pointer.disable_acceleration is a bool, always valid.
            merged_p.pointer = p.pointer.clone();
            self.mouse.profiles.push(merged_p);
        }
        // 迁移完后清掉自身的旧字段(防止序列化出冗余)。
        // After merge, clear our own legacy fields (avoid serializing cruft).
        self.mouse.reverse_scroll = None;
        self.mouse.scroll_mode = None;
        self.mouse.line_count = None;
        self.mouse.pointer = None;
    }

    fn merge_colors(ours: &mut ThemeColors, theirs: &ThemeColors, theme: &str, errs: &[String]) {
        let p = format!("colors.{theme}");
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.status_bar_text")))
        {
            ours.status_bar_text = theirs.status_bar_text.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.app_name"))) {
            ours.app_name = theirs.app_name.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.win_title")))
        {
            ours.win_title = theirs.win_title.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.icon_inner_bg")))
        {
            ours.icon_inner_bg = theirs.icon_inner_bg.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.icon_text")))
        {
            ours.icon_text = theirs.icon_text.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.card_bg_sel")))
        {
            ours.card_bg_sel = theirs.card_bg_sel.clone();
        }
        if !errs
            .iter()
            .any(|e| e.starts_with(&format!("{p}.card_border_sel")))
        {
            ours.card_border_sel = theirs.card_border_sel.clone();
        }
    }
}

// ========== Load / Save ==========

fn config_path() -> std::path::PathBuf {
    config_path_in(&std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

/// 在给定 home 下计算配置路径(纯函数,测试可注入临时目录)。
/// Compute the config path under a given home (pure; tests inject a temp dir).
fn config_path_in(home: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(home).join(".config/oh-my-tab");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("config.toml")
}

/// Parse hex string like "999999ff" → u32 0x999999ff.
pub fn parse_hex8(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap_or(0)
}

impl Config {
    /// 把当前配置序列化为 TOML 写回 `~/.config/oh-my-tab/config.toml`。
    /// Serialize this config to TOML and write it back to the config file.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        self.save_to(&path)
    }

    /// 序列化到指定路径(纯逻辑,测试注入临时目录)。
    /// Serialize to a given path (pure logic; tests inject a temp dir).
    fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| format!("serialize config: {}", e))?;
        std::fs::write(path, toml_str).map_err(|e| format!("write {}: {}", path.display(), e))?;
        Ok(())
    }

    pub fn load_or_default() -> (Self, Vec<String>) {
        let path = config_path();
        Self::load_or_default_from(&path)
    }

    /// 从指定路径加载(纯逻辑,测试注入临时目录)。默认路径为 `~/.config/oh-my-tab/config.toml`。
    /// Load from a given path (pure logic; tests inject a temp dir).
    fn load_or_default_from(path: &std::path::Path) -> (Self, Vec<String>) {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let mut loaded: Config = toml::from_str(&content).unwrap_or_default();
                // 迁移旧 [mouse] 扁平字段为 profiles(幂等);返回值 = 是否有改动,
                // 有改动才写回磁盘(新格式配置不再因 serde 兜底值误触发写盘)。
                // Migrate legacy flat [mouse] fields into profiles (idempotent); the return
                // value signals a change so only actual changes rewrite the file (new-format
                // configs no longer trigger a rewrite via serde-backfilled defaults).
                let needs_persist = loaded.mouse.migrate_legacy();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    // Start from defaults, merge only valid fields
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    let _ = merged.save_to(path);
                    (merged, errs)
                } else {
                    // 迁移后写回磁盘(一次性,之后 migrate_legacy 不再改动)。
                    // Persist the migrated config (one-time; migrate_legacy is a no-op afterwards).
                    if needs_persist {
                        let _ = loaded.save_to(path);
                    }
                    (loaded, Vec::new())
                }
            }
            Err(_) => {
                // File doesn't exist — write defaults
                let defaults = Config::default();
                if let Ok(toml_str) = toml::to_string_pretty(&defaults) {
                    let _ = std::fs::write(path, toml_str);
                }
                (defaults, Vec::new())
            }
        }
    }

    pub fn reload() -> (Self, Vec<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let mut loaded: Config = toml::from_str(&content).unwrap_or_default();
                let needs_persist = loaded.mouse.migrate_legacy();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    let _ = merged.save_to(&path);
                    (merged, errs)
                } else {
                    if needs_persist {
                        let _ = loaded.save_to(&path);
                    }
                    (loaded, Vec::new())
                }
            }
            Err(e) => {
                let defaults = Config::default();
                (
                    defaults,
                    vec![tf(
                        "errors.config_read_failed",
                        &[("error", &e.to_string())],
                    )],
                )
            }
        }
    }
}

// ========== Global singleton ==========

pub static CONFIG: std::sync::LazyLock<RwLock<Config>> = std::sync::LazyLock::new(|| {
    let (cfg, _errs) = Config::load_or_default();
    // 应用 config 里的 locale 覆盖(I18N 初始化时只用了系统语言)。
    // 无循环:I18N 不读 CONFIG,见 i18n.rs 文件头说明。
    // Apply the locale from config (I18N init only used the system locale).
    // No cycle: I18N does not read CONFIG; see the note at the top of i18n.rs.
    i18n::apply_config_locale(&cfg.i18n.locale);
    RwLock::new(cfg)
});

/// Reload config from disk and apply. Returns validation errors (empty = success).
pub fn reload_config() -> Vec<String> {
    let (new_cfg, errs) = Config::reload();
    let locale = new_cfg.i18n.locale.clone();
    let log_level = new_cfg.logging.level.clone();
    if let Ok(mut cfg) = CONFIG.write() {
        *cfg = new_cfg;
    }
    // locale 可能随 reload 改变,重新应用 / locale may change on reload, re-apply
    i18n::apply_config_locale(&locale);
    // 热更新日志级别 / hot-reload log level
    let lvl = match log_level.as_str() {
        "debug" => crate::logger::LogLevel::Debug,
        _ => crate::logger::LogLevel::Info,
    };
    crate::logger::reconfigure(lvl);
    // 配置变更:失效 per-device 解析缓存(下次 resolve 重新合并 profiles)。
    // Config changed: invalidate the per-device resolve cache (next resolve re-merges profiles).
    crate::mouse::resolve::invalidate_cache();
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== parse_hex8 ==========

    #[test]
    fn parse_hex8_parses_valid_rgb_alpha() {
        assert_eq!(parse_hex8("999999ff"), 0x999999ff);
        assert_eq!(parse_hex8("00000000"), 0x00000000);
        assert_eq!(parse_hex8("FFFFFFFF"), 0xffffffff);
        assert_eq!(parse_hex8("12345678"), 0x12345678);
    }

    #[test]
    fn parse_hex8_invalid_inputs_fall_back_to_zero() {
        // 非法/空串/超长统一回退 0,绝不 panic。
        // Invalid/empty/overlong inputs all fall back to 0, never panic.
        assert_eq!(parse_hex8(""), 0);
        assert_eq!(parse_hex8("xyz"), 0);
        assert_eq!(parse_hex8("999999999"), 0); // 9 位溢出 / 9 chars overflows u32
        assert_eq!(parse_hex8("gggggggg"), 0);
    }

    // ========== validate ==========

    fn assert_err_count(cfg: &Config, expected: usize) {
        let errs = cfg.validate();
        assert_eq!(errs.len(), expected, "errors: {:?}", errs);
    }

    #[test]
    fn defaults_validate_clean() {
        assert_err_count(&Config::default(), 0);
    }

    #[test]
    fn validate_catches_appearance_theme() {
        let mut cfg = Config::default();
        cfg.appearance.theme = "neon".into();
        assert_err_count(&cfg, 1);
    }

    #[test]
    fn validate_catches_bad_hex_colors() {
        let mut cfg = Config::default();
        cfg.colors.dark.app_name = "nothex".into();
        cfg.colors.light.card_bg_sel = "12345".into();
        assert_err_count(&cfg, 2);
    }

    #[test]
    fn validate_catches_layout_ranges() {
        let mut cfg = Config::default();
        cfg.layout.cards_per_row = 0;
        cfg.layout.card_width = 10.0;
        cfg.layout.card_height = 50.0;
        cfg.layout.card_gap = -1.0;
        cfg.layout.icon_size = 10.0;
        assert_err_count(&cfg, 5);
    }

    #[test]
    fn validate_catches_keyboard_and_locale() {
        let mut cfg = Config::default();
        cfg.keyboard.modifier = "ctrl".into();
        cfg.i18n.locale = "fr".into();
        cfg.logging.level = "verbose".into();
        cfg.windows.overlay_position = "nowhere".into();
        assert_err_count(&cfg, 4);
    }

    #[test]
    fn validate_catches_mouse_profile_ranges() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.push(MouseProfile {
            scroll_mode: Some("turbo".into()),
            line_count: Some(99),
            ..Default::default()
        });
        assert_err_count(&cfg, 2);
    }

    // ========== merge_valid ==========

    #[test]
    fn merge_valid_keeps_healthy_sections_wholesale() {
        // 无错误的配置:整体并入,所有自定义值保留(曾用默认值合并默认值,断言恒真)。
        // A fully valid config is merged wholesale with all custom values kept (the old test
        // merged defaults into defaults, making the assertions vacuous).
        let mut other = Config::default();
        other.appearance.theme = "dark".into();
        other.layout.cards_per_row = 3;
        other.keyboard.modifier = "option".into();
        other.i18n.locale = "zh-Hant".into();
        other.mouse.enabled = true;
        other.clipboard.enabled = true;
        other.clipboard.max_entries = 30;
        let mut merged = Config::default();
        merged.merge_valid(other, &[]);
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(merged.layout.cards_per_row, 3);
        assert_eq!(merged.keyboard.modifier, "option");
        assert_eq!(merged.i18n.locale, "zh-Hant");
        assert!(merged.mouse.enabled);
        assert!(merged.clipboard.enabled);
        assert_eq!(merged.clipboard.max_entries, 30);
    }

    #[test]
    fn validate_rejects_out_of_range_clipboard_max_entries() {
        // 剪贴板最大条数必须在 1..=100 内。
        // Clipboard max entries must be within 1..=100.
        let mut cfg = Config::default();
        cfg.clipboard.max_entries = 0;
        assert_err_count(&cfg, 1);
        cfg.clipboard.max_entries = 101;
        assert_err_count(&cfg, 1);
        cfg.clipboard.max_entries = 50;
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn merge_valid_resets_only_invalid_fields() {
        let mut other = Config::default();
        // 合法字段:全部自定义。
        other.appearance.theme = "dark".into();
        other.appearance.glass_style = "regular".into();
        other.appearance.glass_tint = "11223344".into();
        other.appearance.corner_radius = 12.0;
        // 非法字段:corner_radius < 0。
        let mut cfg = other.clone();
        cfg.appearance.corner_radius = -5.0;
        let errs = cfg.validate();
        assert_eq!(errs.len(), 1);

        let mut merged = Config::default();
        merged.merge_valid(cfg, &errs);
        // 合法字段保留,非法字段回落默认。
        // Valid fields survive; the invalid one falls back to the default.
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(merged.appearance.glass_tint, "11223344");
        assert_eq!(
            merged.appearance.corner_radius,
            Config::default().appearance.corner_radius
        );
    }

    #[test]
    fn merge_valid_field_level_merges_inside_erroring_section() {
        // 同一 section 内合法/非法字段混合:只重置非法者。
        // Mixed valid/invalid fields within one section: only the invalid one is reset.
        let mut cfg = Config::default();
        cfg.layout.cards_per_row = 3; // 合法 / valid
        cfg.layout.card_width = 1.0; // 非法 / invalid
        let errs = cfg.validate();
        assert_eq!(errs.len(), 1);

        let mut merged = Config::default();
        merged.merge_valid(cfg, &errs);
        assert_eq!(merged.layout.cards_per_row, 3);
        assert_eq!(
            merged.layout.card_width,
            Config::default().layout.card_width
        );
    }

    // ========== mouse migration ==========

    #[test]
    fn migrate_legacy_is_idempotent() {
        let mut cfg = Config::default();
        let before = cfg.mouse.clone();
        cfg.mouse.migrate_legacy();
        cfg.mouse.migrate_legacy();
        // 二次迁移不应改变任何内容。
        // A second migration must not change anything.
        assert_eq!(cfg.mouse.profiles.len(), before.profiles.len());
        assert!(cfg.mouse.reverse_scroll.is_none());
    }

    #[test]
    fn migrate_legacy_folds_legacy_fields_into_wildcard_profile() {
        let mut cfg = Config::default();
        cfg.mouse.profiles.clear();
        cfg.mouse.reverse_scroll = Some(true);
        cfg.mouse.scroll_mode = Some("line".into());
        cfg.mouse.line_count = Some(7);
        cfg.mouse.pointer = Some(PointerSection {
            disable_acceleration: true,
        });
        let changed = cfg.mouse.migrate_legacy();
        assert!(changed);
        // 合并进"所有鼠标"档,旧字段清空。
        // Folded into the "All Mice" profile; legacy fields cleared.
        assert_eq!(cfg.mouse.profiles.len(), 1);
        let p = &cfg.mouse.profiles[0];
        assert_eq!(p.reverse_scroll, Some(true));
        assert_eq!(p.scroll_mode.as_deref(), Some("line"));
        assert_eq!(p.line_count, Some(7));
        assert_eq!(
            p.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        assert!(cfg.mouse.reverse_scroll.is_none());
        assert!(cfg.mouse.scroll_mode.is_none());
    }

    // ========== save / load roundtrip ==========

    #[test]
    fn save_and_load_roundtrip_preserves_custom_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        // 全部使用非默认自定义值——曾用默认值 roundtrip 掩盖了 migrate_legacy
        // 覆盖新格式 profiles 的 bug(见 docs/test-review.md),这里做回归防护。
        // All fields set to non-default values -- the old default-value roundtrip masked the
        // migrate_legacy overwrite bug (see docs/test-review.md); this is the regression guard.
        cfg.appearance.theme = "dark".into();
        cfg.appearance.glass_tint = "11223344".into();
        cfg.layout.cards_per_row = 3;
        cfg.layout.card_width = 200.0;
        cfg.layout.card_height = 240.0;
        cfg.keyboard.modifier = "option".into();
        cfg.i18n.locale = "zh-Hans".into();
        cfg.windows.overlay_position = "main".into();
        cfg.mouse.enabled = true;
        cfg.mouse.profiles = vec![
            MouseProfile {
                reverse_scroll: Some(true),
                scroll_mode: Some("line".into()),
                line_count: Some(5),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(true),
                }),
                ..Default::default()
            },
            MouseProfile {
                device: DeviceMatcher {
                    vendor_id: Some(1133),
                    product_id: Some(17492),
                },
                reverse_scroll: Some(false),
                ..Default::default()
            },
        ];
        cfg.save_to(&path).unwrap();

        let (loaded, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        // 非 mouse 字段 roundtrip。
        // Non-mouse fields survive the roundtrip.
        assert_eq!(loaded.appearance.theme, "dark");
        assert_eq!(loaded.appearance.glass_tint, "11223344");
        assert_eq!(loaded.layout.cards_per_row, 3);
        assert_eq!(loaded.keyboard.modifier, "option");
        assert_eq!(loaded.i18n.locale, "zh-Hans");
        assert_eq!(loaded.windows.overlay_position, "main");
        // mouse profiles 原样保留(通配档 + per-device 档各一条)。
        // Mouse profiles survive untouched (one wildcard + one per-device).
        assert!(loaded.mouse.enabled);
        assert_eq!(loaded.mouse.profiles.len(), 2);
        let w = &loaded.mouse.profiles[0];
        assert_eq!(w.device.vendor_id, None);
        assert_eq!(w.reverse_scroll, Some(true));
        assert_eq!(w.scroll_mode.as_deref(), Some("line"));
        assert_eq!(w.line_count, Some(5));
        assert_eq!(
            w.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        let d = &loaded.mouse.profiles[1];
        assert_eq!(d.device.vendor_id, Some(1133));
        assert_eq!(d.device.product_id, Some(17492));
        assert_eq!(d.reverse_scroll, Some(false));
    }

    #[test]
    fn load_new_format_config_leaves_profiles_untouched() {
        // 反向约束:新格式配置(只有 profiles、无旧扁平字段)加载后必须原样保留,
        // 不能被 migrate_legacy 当作"含旧字段"覆盖(曾经的 bug)。
        // Negative constraint: a new-format config (profiles only, no legacy flat fields) must
        // load untouched -- migrate_legacy must not mistake it for legacy content (the old bug).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true

[[mouse.profiles]]
reverse_scroll = true
scroll_mode = "line"
line_count = 5

[mouse.profiles.pointer]
disable_acceleration = true

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 17492
reverse_scroll = false
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        assert_eq!(cfg.mouse.profiles.len(), 2);
        // 通配档:自定义值保留。
        // Wildcard profile: custom values preserved.
        let w = &cfg.mouse.profiles[0];
        assert_eq!(w.reverse_scroll, Some(true));
        assert_eq!(w.scroll_mode.as_deref(), Some("line"));
        assert_eq!(w.line_count, Some(5));
        assert_eq!(
            w.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
        // per-device 档:保留。
        // Per-device profile preserved.
        let d = &cfg.mouse.profiles[1];
        assert_eq!(d.device.vendor_id, Some(1133));
        assert_eq!(d.device.product_id, Some(17492));
        assert_eq!(d.reverse_scroll, Some(false));
    }

    #[test]
    fn load_missing_file_writes_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let (cfg, errs) = Config::load_or_default_from(&path);
        // 不存在的文件:写默认配置并返回之,无错误。
        // Missing file: defaults written and returned, no errors.
        assert!(errs.is_empty());
        assert_eq!(cfg.appearance.theme, "light");
        assert!(path.exists());
    }

    #[test]
    fn load_invalid_file_merges_valid_fields_and_reports_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[appearance]
theme = "dark"
glass_tint = "zzzzzzzz"
[layout]
cards_per_row = 5
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(!errs.is_empty());
        // 合法字段(theme/cards_per_row)保留,非法字段(glass_tint)回落默认。
        // Valid fields survive; the invalid one falls back to the default.
        assert_eq!(cfg.appearance.theme, "dark");
        assert_eq!(cfg.layout.cards_per_row, 5);
        assert_eq!(
            cfg.appearance.glass_tint,
            Config::default().appearance.glass_tint
        );
    }

    #[test]
    fn load_legacy_flat_mouse_fields_migrates_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true
reverse_scroll = true
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        // 旧字段被迁移成"所有鼠标"档并写回磁盘。
        // Legacy fields migrated into an "All Mice" profile and persisted.
        assert_eq!(cfg.mouse.profiles.len(), 1);
        assert_eq!(cfg.mouse.profiles[0].reverse_scroll, Some(true));
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("device_vendor_id") == false);
        assert!(persisted.contains("reverse_scroll"));
    }

    #[test]
    fn config_path_in_uses_home_and_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = config_path_in(dir.path().to_str().unwrap());
        // 路径:home/.config/oh-my-tab/config.toml,目录自动创建。
        // Path: home/.config/oh-my-tab/config.toml with the dir auto-created.
        assert_eq!(p, dir.path().join(".config/oh-my-tab/config.toml"));
        assert!(p.parent().unwrap().exists());
    }
}
