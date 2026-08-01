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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct WindowsSection {
    // 默认 false(不显示最小化窗口,与历史行为一致);bool::default() 即 false,故 Default 可直接派生。
    // Defaults to false (hide minimized windows, matching prior behavior); bool::default() is
    // false, so Default can be derived directly.
    pub show_minimized: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LoggingSection {
    // 日志级别:"trace","debug","info","warn","error";默认 "info"。
    // Log level; default "info".
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
    pub line_count: Option<u32>,
    pub smooth_preset: Option<String>,
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
    // ---- Legacy fields (read for migration only, skipped on serialize) ----
    #[serde(skip_serializing)]
    pub reverse_scroll: bool,
    #[serde(skip_serializing)]
    pub scroll_mode: String,
    #[serde(skip_serializing)]
    pub line_count: u32,
    #[serde(skip_serializing)]
    pub smooth_preset: String,
    #[serde(skip_serializing)]
    pub pointer: PointerSection,
}

impl Default for MouseSection {
    fn default() -> Self {
        Self {
            enabled: false,
            // 默认含一个"所有鼠标"档,与旧默认值一致(reverse_scroll=false, default 模式, 3 行, easeInOut)。
            // Default includes an "All Mice" profile matching the old defaults.
            profiles: vec![MouseProfile {
                reverse_scroll: Some(false),
                scroll_mode: Some("default".into()),
                line_count: Some(3),
                smooth_preset: Some("easeInOut".into()),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(false),
                }),
                ..Default::default()
            }],
            reverse_scroll: false,
            scroll_mode: "default".into(),
            line_count: 3,
            smooth_preset: "easeInOut".into(),
            pointer: PointerSection::default(),
        }
    }
}

impl MouseSection {
    /// 若旧字段被填充(profiles 为空或缺少默认档),把旧字段迁移成一个"所有鼠标"档。
    /// 迁移是幂等的:已迁移的配置(无旧字段、有默认档)不会重复迁移。
    ///
    /// Migrate legacy flat fields into an "All Mice" profile. Idempotent: an already-migrated
    /// config (no legacy fields, has a default profile) is left untouched.
    pub(crate) fn migrate_legacy(&mut self) {
        // 判断旧字段是否"有内容"(与代码默认不同,或 scroll_mode 非空)。
        // Detect whether legacy fields carry meaningful content (differ from code defaults, or
        // scroll_mode is non-empty).
        let defaults = MouseSection::default();
        let has_legacy = self.reverse_scroll != defaults.reverse_scroll
            || self.scroll_mode != defaults.scroll_mode
            || self.line_count != defaults.line_count
            || self.smooth_preset != defaults.smooth_preset
            || self.pointer.disable_acceleration != defaults.pointer.disable_acceleration
            || !self.scroll_mode.is_empty()
            || !self.smooth_preset.is_empty();

        if !has_legacy {
            // 无旧字段(全新配置或已迁移):确保至少有一个默认"所有鼠标"档。
            // No legacy content (fresh or already migrated): ensure a default "All Mice" profile exists.
            let has_default = self
                .profiles
                .iter()
                .any(|p| p.device.vendor_id.is_none() && p.device.product_id.is_none());
            if !has_default {
                self.profiles.insert(0, defaults.profiles[0].clone());
            }
            return;
        }

        // 有旧字段:把它们并入(或新建)一个"所有鼠标"档。
        // Legacy fields present: fold them into (or create) an "All Mice" profile.
        let legacy_profile = MouseProfile {
            reverse_scroll: Some(self.reverse_scroll),
            scroll_mode: Some(if self.scroll_mode.is_empty() {
                "default".into()
            } else {
                self.scroll_mode.clone()
            }),
            line_count: Some(if self.line_count == 0 { 3 } else { self.line_count }),
            smooth_preset: Some(if self.smooth_preset.is_empty() {
                "easeInOut".into()
            } else {
                self.smooth_preset.clone()
            }),
            pointer: Some(PartialPointerSection {
                disable_acceleration: Some(self.pointer.disable_acceleration),
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
        self.reverse_scroll = false;
        self.scroll_mode = String::new();
        self.line_count = 0;
        self.smooth_preset = String::new();
        self.pointer = PointerSection::default();
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
        if !["info", "warn", "error"].contains(&self.logging.level.as_str()) {
            errs.push(tf(
                "errors.logging_level_invalid",
                &[("value", &self.logging.level)],
            ));
        }

        // --- mouse profiles ---
        for (i, p) in self.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            if let Some(ref mode) = p.scroll_mode {
                if !["default", "line", "smooth"].contains(&mode.as_str()) {
                    errs.push(tf(
                        "errors.mouse_scroll_mode_invalid",
                        &[("value", mode)],
                    ));
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
            if let Some(ref sp) = p.smooth_preset {
                if ![
                    "custom",
                    "linear",
                    "easeIn",
                    "easeOut",
                    "easeInOut",
                    "quadratic",
                    "cubic",
                    "quartic",
                    "easeOutCubic",
                    "easeInOutCubic",
                    "easeOutQuartic",
                    "easeInOutQuartic",
                    "smooth",
                ]
                .contains(&sp.as_str())
                {
                    let msg = tf(
                        "errors.mouse_smooth_preset_invalid",
                        &[("value", sp)],
                    );
                    errs.push(format!("{prefix}.smooth_preset: {msg}"));
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

        // windows (bool 字段无需校验,恒有效)
        // windows (bool field needs no validation, always valid)
        self.windows = other.windows;

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
            if p.smooth_preset.is_some()
                && !errs
                    .iter()
                    .any(|e| e.starts_with(&format!("{prefix}.smooth_preset")))
            {
                merged_p.smooth_preset = p.smooth_preset.clone();
            }
            // pointer.disable_acceleration 是 bool,恒有效。
            // pointer.disable_acceleration is a bool, always valid.
            merged_p.pointer = p.pointer.clone();
            self.mouse.profiles.push(merged_p);
        }
        // 迁移完后清掉自身的旧字段(防止序列化出冗余)。
        // After merge, clear our own legacy fields (avoid serializing cruft).
        self.mouse.reverse_scroll = false;
        self.mouse.scroll_mode = String::new();
        self.mouse.line_count = 0;
        self.mouse.smooth_preset = String::new();
        self.mouse.pointer = PointerSection::default();
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
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
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| format!("serialize config: {}", e))?;
        let path = config_path();
        std::fs::write(&path, toml_str).map_err(|e| format!("write {}: {}", path.display(), e))?;
        Ok(())
    }

    pub fn load_or_default() -> (Self, Vec<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let mut loaded: Config = toml::from_str(&content).unwrap_or_default();
                // 迁移旧 [mouse] 扁平字段为 profiles(幂等);若发生了迁移,稍后写回磁盘。
                // Migrate legacy flat [mouse] fields into profiles (idempotent); persist later if changed.
                let needs_persist = !loaded.mouse.profiles.iter().any(|p| {
                    p.device.vendor_id.is_none() && p.device.product_id.is_none()
                }) || loaded.mouse.reverse_scroll
                    || !loaded.mouse.scroll_mode.is_empty()
                    || loaded.mouse.line_count != 0
                    || !loaded.mouse.smooth_preset.is_empty();
                loaded.mouse.migrate_legacy();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    // Start from defaults, merge only valid fields
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    let _ = merged.save();
                    (merged, errs)
                } else {
                    // 迁移后写回磁盘(一次性,之后 migrate_legacy 不再改动)。
                    // Persist the migrated config (one-time; migrate_legacy is a no-op afterwards).
                    if needs_persist {
                        let _ = loaded.save();
                    }
                    (loaded, Vec::new())
                }
            }
            Err(_) => {
                // File doesn't exist — write defaults
                let defaults = Config::default();
                if let Ok(toml_str) = toml::to_string_pretty(&defaults) {
                    let _ = std::fs::write(&path, toml_str);
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
                let needs_persist = !loaded.mouse.profiles.iter().any(|p| {
                    p.device.vendor_id.is_none() && p.device.product_id.is_none()
                }) || loaded.mouse.reverse_scroll
                    || !loaded.mouse.scroll_mode.is_empty()
                    || loaded.mouse.line_count != 0
                    || !loaded.mouse.smooth_preset.is_empty();
                loaded.mouse.migrate_legacy();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    let _ = merged.save();
                    (merged, errs)
                } else {
                    if needs_persist {
                        let _ = loaded.save();
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
        "warn" => crate::logger::LogLevel::Warn,
        "error" => crate::logger::LogLevel::Error,
        _ => crate::logger::LogLevel::Info,
    };
    crate::logger::reconfigure(lvl);
    // 配置变更:失效 per-device 解析缓存(下次 resolve 重新合并 profiles)。
    // Config changed: invalidate the per-device resolve cache (next resolve re-merges profiles).
    crate::mouse::resolve::invalidate_cache();
    errs
}
