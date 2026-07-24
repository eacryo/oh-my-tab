use serde::{Deserialize, Serialize};
use std::sync::RwLock;

// ========== Structs ==========

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    pub layout: Layout,
    pub colors: ColorsSection,
    pub fonts: Fonts,
    pub keyboard: Keyboard,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub glass_style: String,
    pub glass_tint: String,
    pub corner_radius: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Layout {
    pub cards_per_row: usize,
    pub card_width: f64,
    pub card_height: f64,
    pub card_gap: f64,
    pub icon_size: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ColorsSection {
    pub dark: ThemeColors,
    pub light: ThemeColors,
}

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Fonts {
    pub status_bar_size: f64,
    pub status_bar_weight: f64,
    pub title_size: f64,
    pub title_weight: f64,
    pub app_name_size: f64,
    pub app_name_weight: f64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Keyboard {
    pub modifier: String,
}

// ========== Default implementations (hard-coded fallback values) ==========

impl Default for Config {
    fn default() -> Self {
        Config {
            appearance: Appearance::default(),
            layout: Layout::default(),
            colors: ColorsSection::default(),
            fonts: Fonts::default(),
            keyboard: Keyboard::default(),
        }
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            theme: "light".into(),
            glass_style: "clear".into(),
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
            modifier: "option".into(),
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
            errs.push(format!(
                "appearance.theme: \"{}\" is not one of dark, light, auto",
                self.appearance.theme
            ));
        }
        if !["regular", "clear"].contains(&self.appearance.glass_style.as_str()) {
            errs.push(format!(
                "appearance.glass_style: \"{}\" is not one of regular, clear",
                self.appearance.glass_style
            ));
        }
        if !is_hex8(&self.appearance.glass_tint) {
            errs.push(format!(
                "appearance.glass_tint: \"{}\" must be 8 hex digits (RRGGBBAA)",
                self.appearance.glass_tint
            ));
        }
        if self.appearance.corner_radius < 0.0 {
            errs.push(format!(
                "appearance.corner_radius: {} must be >= 0",
                self.appearance.corner_radius
            ));
        }

        // --- layout ---
        if self.layout.cards_per_row < 1 || self.layout.cards_per_row > 10 {
            errs.push(format!(
                "layout.cards_per_row: {} must be 1..=10",
                self.layout.cards_per_row
            ));
        }
        if self.layout.card_width < 80.0 {
            errs.push(format!(
                "layout.card_width: {} must be >= 80",
                self.layout.card_width
            ));
        }
        if self.layout.card_height < 100.0 {
            errs.push(format!(
                "layout.card_height: {} must be >= 100",
                self.layout.card_height
            ));
        }
        if self.layout.card_gap < 0.0 {
            errs.push(format!(
                "layout.card_gap: {} must be >= 0",
                self.layout.card_gap
            ));
        }
        if self.layout.icon_size < 32.0 {
            errs.push(format!(
                "layout.icon_size: {} must be >= 32",
                self.layout.icon_size
            ));
        }

        // --- colors ---
        for (theme, colors) in [("dark", &self.colors.dark), ("light", &self.colors.light)] {
            let prefix = format!("colors.{theme}");
            if !is_hex8(&colors.status_bar_text) {
                errs.push(format!("{prefix}.status_bar_text: must be 8 hex digits"));
            }
            if !is_hex8(&colors.app_name) {
                errs.push(format!("{prefix}.app_name: must be 8 hex digits"));
            }
            if !is_hex8(&colors.win_title) {
                errs.push(format!("{prefix}.win_title: must be 8 hex digits"));
            }
            if !is_hex8(&colors.icon_inner_bg) {
                errs.push(format!("{prefix}.icon_inner_bg: must be 8 hex digits"));
            }
            if !is_hex8(&colors.icon_text) {
                errs.push(format!("{prefix}.icon_text: must be 8 hex digits"));
            }
            if !is_hex8(&colors.card_bg_sel) {
                errs.push(format!("{prefix}.card_bg_sel: must be 8 hex digits"));
            }
            if !is_hex8(&colors.card_border_sel) {
                errs.push(format!("{prefix}.card_border_sel: must be 8 hex digits"));
            }
        }

        // --- fonts ---
        if self.fonts.status_bar_size < 8.0 {
            errs.push(format!("fonts.status_bar_size: {} must be >= 8", self.fonts.status_bar_size));
        }
        if self.fonts.status_bar_weight < 0.0 || self.fonts.status_bar_weight > 1.0 {
            errs.push(format!("fonts.status_bar_weight: {} must be 0.0..=1.0", self.fonts.status_bar_weight));
        }
        if self.fonts.title_size < 8.0 {
            errs.push(format!("fonts.title_size: {} must be >= 8", self.fonts.title_size));
        }
        if self.fonts.title_weight < 0.0 || self.fonts.title_weight > 1.0 {
            errs.push(format!("fonts.title_weight: {} must be 0.0..=1.0", self.fonts.title_weight));
        }
        if self.fonts.app_name_size < 8.0 {
            errs.push(format!("fonts.app_name_size: {} must be >= 8", self.fonts.app_name_size));
        }
        if self.fonts.app_name_weight < 0.0 || self.fonts.app_name_weight > 1.0 {
            errs.push(format!("fonts.app_name_weight: {} must be 0.0..=1.0", self.fonts.app_name_weight));
        }

        // --- keyboard ---
        if !["option", "command"].contains(&self.keyboard.modifier.as_str()) {
            errs.push(format!(
                "keyboard.modifier: \"{}\" is not one of option, command",
                self.keyboard.modifier
            ));
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
            if !errs.iter().any(|e| e.starts_with("appearance.corner_radius")) {
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
            if !errs.iter().any(|e| e.starts_with("fonts.status_bar_weight")) {
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
    }

    fn merge_colors(ours: &mut ThemeColors, theirs: &ThemeColors, theme: &str, errs: &[String]) {
        let p = format!("colors.{theme}");
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.status_bar_text"))) {
            ours.status_bar_text = theirs.status_bar_text.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.app_name"))) {
            ours.app_name = theirs.app_name.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.win_title"))) {
            ours.win_title = theirs.win_title.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.icon_inner_bg"))) {
            ours.icon_inner_bg = theirs.icon_inner_bg.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.icon_text"))) {
            ours.icon_text = theirs.icon_text.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.card_bg_sel"))) {
            ours.card_bg_sel = theirs.card_bg_sel.clone();
        }
        if !errs.iter().any(|e| e.starts_with(&format!("{p}.card_border_sel"))) {
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
    pub fn load_or_default() -> (Self, Vec<String>) {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let loaded: Config = toml::from_str(&content).unwrap_or_default();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    // Start from defaults, merge only valid fields
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    (merged, errs)
                } else {
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
                let loaded: Config = toml::from_str(&content).unwrap_or_default();
                let errs = loaded.validate();
                if !errs.is_empty() {
                    let mut merged = Config::default();
                    merged.merge_valid(loaded, &errs);
                    (merged, errs)
                } else {
                    (loaded, Vec::new())
                }
            }
            Err(e) => {
                let defaults = Config::default();
                (defaults, vec![format!("cannot read config: {}", e)])
            }
        }
    }
}

// ========== Global singleton ==========

pub static CONFIG: std::sync::LazyLock<RwLock<Config>> =
    std::sync::LazyLock::new(|| {
        let (cfg, _errs) = Config::load_or_default();
        RwLock::new(cfg)
    });

/// Reload config from disk and apply. Returns validation errors (empty = success).
pub fn reload_config() -> Vec<String> {
    let (new_cfg, errs) = Config::reload();
    if let Ok(mut cfg) = CONFIG.write() {
        *cfg = new_cfg;
    }
    errs
}
