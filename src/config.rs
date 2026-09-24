use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, RwLock};

use crate::i18n::{self, tf};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
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
    pub updates: UpdatesSection,
    pub clipboard: ClipboardSection,
    pub mouse: MouseSection,
    pub window_control: WindowControlSection,
    pub quick_actions: QuickActionsSection,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct QuickActionsSection {
    // Quick-actions master switch (Option+I/E/D/L and double-Control global hotkeys). Default false: the global
    // interception overrides other apps' Option+letters (dead keys / special chars on some
    // layouts), so it must be explicitly opted in.
    pub enabled: bool,
    // Sub-switches default to true so existing configs with only enabled=true keep working.
    #[serde(default = "default_quick_action_enabled")]
    pub open_settings: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub open_finder: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub show_desktop: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub lock_screen: bool,
    #[serde(default = "default_quick_action_enabled")]
    pub locate_pointer: bool,
}

fn default_quick_action_enabled() -> bool {
    true
}

impl Default for QuickActionsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            open_settings: true,
            open_finder: true,
            show_desktop: true,
            lock_screen: true,
            locate_pointer: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WindowControlSection {
    // Window-control master switch (Option + arrow keys). Default false: the global
    // interception overrides other apps' Option+arrows (e.g. move-by-word in text), so it
    // must be explicitly opted in.
    pub enabled: bool,
    // Direction switches default to true so existing configs with only enabled=true keep working.
    #[serde(default = "default_window_control_direction_enabled")]
    pub up: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub down: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub left: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub right: bool,
    // Cross-display shortcuts default to enabled, matching the direction shortcuts.
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_up: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_down: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_left: bool,
    #[serde(default = "default_window_control_direction_enabled")]
    pub display_right: bool,
}

fn default_window_control_direction_enabled() -> bool {
    true
}

impl Default for WindowControlSection {
    fn default() -> Self {
        Self {
            enabled: false,
            up: true,
            down: true,
            left: true,
            right: true,
            display_up: true,
            display_down: true,
            display_left: true,
            display_right: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Appearance {
    pub theme: String,
    pub glass_style: String,
    pub glass_tint: String,
    pub corner_radius: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Layout {
    // Window-thumbnail master switch: off = the overlay keeps icon-only rendering
    // and the thumbnail service never starts. Default on; auto-sleeps without the
    // Screen Recording permission (see the thumbnail module).
    pub thumbnails_enabled: bool,
    // Focused-window thumbnail prewarm: when enabled, refresh the frontmost window at a low rate
    // while the overlay is hidden. Default off to keep background work bounded.
    pub focused_thumbnail_prewarm: bool,
    // App name in card titles: when enabled the thumbnail card's caption shows the app name
    // before the window title, separated by " · "; a titleless window, or one whose title equals
    // the app name, shows a single copy. Default off (window title only).
    pub show_app_name_in_cards: bool,
    // Card text size (points): the window title and app name scale proportionally; the large
    // icon in icon-only mode is unaffected.
    pub card_text_size: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ColorsSection {
    pub dark: ThemeColors,
    pub light: ThemeColors,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Fonts {
    pub status_bar_size: f64,
    pub status_bar_weight: f64,
    pub title_size: f64,
    pub title_weight: f64,
    pub app_name_size: f64,
    pub app_name_weight: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Keyboard {
    pub modifier: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct I18nSection {
    pub locale: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WindowsSection {
    // Master switch for the app switcher: when off, Cmd+Tab passes through to the system
    // (the native switcher takes over) and the tap stops intercepting. Defaults to true.
    pub enabled: bool,
    // Defaults to false (hide minimized windows, matching prior behavior); bool::default() is
    // false, so Default can be derived directly.
    pub show_minimized: bool,
    // Do not show windows belonging to Command+H-hidden apps by default.
    pub show_hidden_app_windows: bool,
    // Overlay display position: "active_window" = follow the active window's screen,
    // "main" = always on the main screen. Defaults to following the active window.
    pub overlay_position: String,
    // Window activation mode: "hover" activates on hover; "click" activates on click.
    // Defaults to hover.
    pub activation_mode: String,
}

impl Default for WindowsSection {
    fn default() -> Self {
        Self {
            enabled: true,
            show_minimized: false,
            show_hidden_app_windows: false,
            overlay_position: "active_window".to_string(),
            activation_mode: "hover".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct LoggingSection {
    // Log level: "debug" | "info"; default "info" (normal tier, no spam; debug emits all detail).
    pub level: String,
    // Log file path; empty = use the default rolling file under ~/Library/Logs/oh-my-tab/.
    pub file_path: String,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct StartupSection {
    // Launch at login; defaults to false (bool::default() is false, so Default derives directly).
    pub launch_at_login: bool,
}

/// Sparkle update settings.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct UpdatesSection {
    /// Whether Sparkle may check for updates in the background.
    pub automatically_check: bool,
    /// Whether Sparkle automatically downloads and installs updates.
    pub automatically_download: bool,
}

impl Default for UpdatesSection {
    fn default() -> Self {
        Self {
            automatically_check: true,
            automatically_download: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ClipboardSection {
    // History-clipboard master switch; defaults to false (no pasteboard polling).
    pub enabled: bool,
    // Max history entries (1..=100, default 50).
    pub max_entries: u32,
    // Show the source app: the source is ALWAYS recorded at copy time (ClipEntry.source_app);
    // this switch only controls whether the row displays the app name. Default false.
    pub show_source_app: bool,
    // Persist the history: when on, text/images (incl. file references) are saved to
    // ~/.config/oh-my-tab/clipboard-history.toml and survive restarts. Default false --
    // plaintext on disk has privacy implications (any same-user app can read it); the
    // README carries an explicit warning.
    pub persist: bool,
    // Move used entries to the top: pasting (select + Enter) brings the entry to the
    // front (a side effect: the write-back is re-captured by the poll as another copy).
    // When off, pasting does not reorder the history (like Windows Win+V). Default true.
    pub move_used_to_top: bool,
    // Delete after paste: when on, Option+click or Option+Enter pastes the entry and
    // removes it from the history right away (one-shot paste). Default false -- a
    // destructive gesture, strictly opt-in.
    pub delete_after_paste: bool,
    // Clear the current system pasteboard after pasting: effective only when
    // delete_after_paste is also enabled. Default false.
    pub clear_system_pasteboard_after_paste: bool,
    // Auto-expire days: unpinned entries older than N days are removed from the history
    // (memory AND persistence); pinned entries never expire. Range 0..=7; 0 = never.
    // Default 3 days.
    pub auto_expire_days: u32,
    // The clipboard picker position: "mouse" = follows the cursor, "main" = centered on
    // the main screen. Defaults to the center of the main screen.
    pub picker_position: String,
    // Where the selection lands after pin/unpin: true = follow the toggled entry to its
    // new position; false = keep the current display position (pointing at the next
    // entry, convenient for batch pinning). Defaults to follow.
    pub pin_follow_selection: bool,
}

impl Default for ClipboardSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_entries: 50,
            show_source_app: false,
            persist: false,
            move_used_to_top: true,
            delete_after_paste: false,
            clear_system_pasteboard_after_paste: false,
            auto_expire_days: 3,
            picker_position: "main".to_string(),
            pin_follow_selection: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PointerSection {
    // Disable system pointer acceleration for 1:1 linear cursor tracking. Default false.
    pub disable_acceleration: bool,
}

/// The valid range for pointer acceleration / tracking speed: **0..=10**.
///
/// The platform property's own domain is [0, 40] ∪ {-1}, but the usable band is only ~0.3-3
/// (macOS ships 1.00 for the mouse key and 0.6875 for the trackpad/pointer key; a value of 10
/// already feels unusably fast), so anything above 10 is
/// wasted travel that also destroys a linear slider's precision. The accepted range is therefore
/// narrowed to 0..=10; should a low-DPI device ever need a larger multiplier, widening this single
/// constant is enough. Semantics: 0 is the bottom of the normal range (slowest, and it really
/// takes effect), -1 is the "acceleration and sensitivity disabled" sentinel (used by the legacy
/// fallback path); only "unset" (None) leaves the device value alone.
pub const MOUSE_ACCELERATION_MIN: f64 = 0.0;
pub const MOUSE_ACCELERATION_MAX: f64 = 10.0;

/// Device matcher (None = wildcard, i.e. "All Mice"). Config matches devices by VID+PID.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct DeviceMatcher {
    // Flattened as device_vendor_id / device_product_id (top-level scalars for hand-written TOML).
    #[serde(rename = "device_vendor_id", skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u32>,
    #[serde(rename = "device_product_id", skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u32>,
    // Virtual-pointer profile: the pointer a software KVM (e.g. Deskflow) injects. It has no HID
    // device and no VID/PID, so it can only be identified by "some other process injected this
    // event" (CGEventSourceUnixProcessID != 0) -- VID/PID matching cannot express it.
    // true = this profile matches injected events only; None = an ordinary device (or wildcard).
    #[serde(rename = "device_injected", skip_serializing_if = "Option::is_none")]
    pub injected: Option<bool>,
}

impl DeviceMatcher {
    /// Whether this matcher is the virtual-pointer one (matches injected events only).
    pub fn is_virtual(&self) -> bool {
        self.injected == Some(true)
    }
}

/// Pointer override (partial; None = inherit from the lower layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct PartialPointerSection {
    pub disable_acceleration: Option<bool>,
    // Tracking speed in linear mode (0..=40), written to HIDPointerAcceleration (IOFixed:
    // value × 65536). Only takes effect while disable_acceleration is on: under linear scaling
    // that property is the tracking speed itself, whereas with the switch off it is the strength
    // of macOS's acceleration curve, a different meaning, so it is not written then.
    // None = leave the device's current value alone.
    pub acceleration: Option<f64>,
}

/// A single profile. device = None is the "All Mice" profile (the default layer).
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct MouseProfile {
    /// Device matcher; None = matches all mice (serves as the default layer).
    #[serde(flatten)]
    pub device: DeviceMatcher,
    // Reverse scroll direction. true = flip relative to the system's current direction (same as
    // LinearMouse; no natural-scroll setting is read: HID-tap events already carry the system
    // natural-scroll flip and synthetic events aren't flipped again, see should_flip in scrolling.rs).
    pub reverse_scroll: Option<bool>,
    pub scroll_mode: Option<String>,
    // Line mode lines per notch (1..=10).
    pub line_count: Option<u32>,
    pub pointer: Option<PartialPointerSection>,
    // Button mappings: button number (string, >= 2) -> shortcut description (e.g. "cmd+shift+v").
    // Left (0) and right (1) buttons cannot be bound, so the user can never lock themselves
    // out of clicking.
    #[serde(default)]
    pub button_mappings: std::collections::HashMap<String, String>,
    // Per-profile master switch for button mappings (None = inherit the lower layer;
    // defaults to true, so bindings take effect as soon as they exist). Independent per
    // device -- different mice can differ. When off, the device's mappings are skipped
    // (events pass through).
    #[serde(default)]
    pub button_mappings_enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct MouseSection {
    // Enable mouse control features (master switch). Default false.
    pub enabled: bool,
    // Profile list: the first profile without a device field is the "All Mice" default layer;
    // subsequent per-device profiles override it. Merge semantics: all matching profiles are
    // traversed, later ones win.
    pub profiles: Vec<MouseProfile>,

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
            // Default includes an "All Mice" profile matching the old defaults.
            profiles: vec![MouseProfile {
                reverse_scroll: Some(false),
                scroll_mode: Some("default".into()),
                line_count: Some(3),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(false),
                    acceleration: None,
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
    /// Migrate legacy flat fields into an "All Mice" profile. Idempotent: an already-migrated
    /// config (no legacy fields, has a default profile) is left untouched. Returns whether
    /// anything changed (legacy fields migrated, or a default profile inserted) -- the caller
    /// uses it to decide whether to rewrite the file.
    pub(crate) fn migrate_legacy(&mut self) -> bool {
        // Legacy fields are Option-typed: only keys explicitly written by old-format files are
        // Some. New-format files skip them on serialize, so they reload as None and no longer
        // trigger a migration that would clobber user profiles (serde used to backfill
        // "default" into the legacy scroll_mode, making has_legacy always true).
        let has_legacy = self.reverse_scroll.is_some()
            || self.scroll_mode.is_some()
            || self.line_count.is_some()
            || self.pointer.is_some();

        if !has_legacy {
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
                ..Default::default()
            }),
            ..Default::default()
        };

        // If an "All Mice" profile already exists, overwrite its fields with the legacy values
        // (the legacy fields are the user's true intent).
        if let Some(idx) = self
            .profiles
            .iter()
            .position(|p| p.device.vendor_id.is_none() && p.device.product_id.is_none())
        {
            self.profiles[idx] = legacy_profile;
        } else {
            // No "All Mice" profile: insert at the front (default first, per-device after).
            self.profiles.insert(0, legacy_profile);
        }

        // Clear legacy fields (prevents re-migration; serialization skips them anyway).
        self.reverse_scroll = None;
        self.scroll_mode = None;
        self.line_count = None;
        self.pointer = None;
        true
    }
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            // Follow the system by default while preserving explicit user choices from existing
            // config files.
            theme: "auto".into(),
            glass_style: "regular".into(),
            // Default Liquid Glass overlay tint (RRGGBBAA); the settings page lets users pick another color.
            glass_tint: "eeeeee66".into(),
            corner_radius: 32.0,
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            thumbnails_enabled: true,
            focused_thumbnail_prewarm: false,
            show_app_name_in_cards: false,
            card_text_size: 15.0,
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
        // Card text colors follow the HTML reference: primary (window title)
        // rgba(0,0,0,.82) -> same alpha in white for the dark theme; secondary (app
        // name) rgba(0,0,0,.34). alpha = 0.82/0.34 x 255 ~= D1/57.
        ThemeColors {
            status_bar_text: "999999ff".into(),
            app_name: "FFFFFF57".into(),
            win_title: "FFFFFFD1".into(),
            icon_inner_bg: "22224444".into(),
            icon_text: "9999bbff".into(),
            card_bg_sel: "22224444".into(),
            card_border_sel: "5577ccff".into(),
        }
    }

    fn light_default() -> Self {
        ThemeColors {
            status_bar_text: "333333ff".into(),
            // rgba(0,0,0,.82) / rgba(0,0,0,.34); see the dark_default comment.
            app_name: "00000057".into(),
            win_title: "000000D1".into(),
            // The mockup's preview background #f6f7f9: a near-white neutral so the
            // preview reads as the same surface as the card.
            icon_inner_bg: "F6F7F9FF".into(),
            icon_text: "666688ff".into(),
            // The mockup's .item.selected background rgba(255,255,255,.88): a clearly
            // visible light surface carrying the caption and preview (the faint accent
            // tint was invisible on the glass panel -- user-reported).
            card_bg_sel: "FFFFFFE0".into(),
            // The mockup's crisp 1.5px selected border rgba(75,123,236,.78) -- the only
            // outline visible against the white surface (a 2px soft ring disappears
            // on white; see the refresh_highlight comment).
            card_border_sel: "4B7BECC7".into(),
        }
    }
}

impl Default for Fonts {
    fn default() -> Self {
        // The two card text lines follow the HTML reference (preview (1).html):
        // primary = window title at 12px / weight 500 (CSS 500 ~= NSFont medium 0.23,
        // not bold); secondary = app name at 10px / regular (CSS omits font-weight =
        // 400 ~= NSFontWeightRegular 0.0).
        Fonts {
            status_bar_size: 15.0,
            status_bar_weight: 0.23,
            title_size: 12.0,
            title_weight: 0.23,
            app_name_size: 10.0,
            app_name_weight: 0.0,
        }
    }
}

impl Default for Keyboard {
    fn default() -> Self {
        Keyboard {
            // Default Cmd+Tab; users can switch back to Option+Tab in Settings.
            modifier: "command".into(),
        }
    }
}

impl Default for I18nSection {
    fn default() -> Self {
        I18nSection {
            locale: "auto".into(), // follow system language
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

fn is_hex8(s: &str) -> bool {
    s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl Config {
    pub fn validate(&self) -> Vec<String> {
        let mut errs: Vec<String> = Vec::new();

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

        if !self.layout.card_text_size.is_finite()
            || !(13.0..=20.0).contains(&self.layout.card_text_size)
        {
            errs.push(tf(
                "errors.layout_card_text_size_invalid",
                &[("value", &self.layout.card_text_size.to_string())],
            ));
        }

        if !self.fonts.status_bar_size.is_finite()
            || !(13.0..=20.0).contains(&self.fonts.status_bar_size)
        {
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

        if !["option", "command"].contains(&self.keyboard.modifier.as_str()) {
            errs.push(tf(
                "errors.keyboard_modifier_invalid",
                &[("value", &self.keyboard.modifier)],
            ));
        }

        let locale_valid =
            ["auto", "en", "zh-Hans", "zh-Hant"].contains(&self.i18n.locale.as_str());
        #[cfg(any(debug_assertions, feature = "dev-long-text"))]
        let locale_valid = locale_valid || self.i18n.locale == i18n::TEST_LONG_LOCALE;
        if !locale_valid {
            errs.push(tf(
                "errors.i18n_locale_invalid",
                &[("value", &self.i18n.locale)],
            ));
        }

        if !["debug", "info"].contains(&self.logging.level.as_str()) {
            errs.push(tf(
                "errors.logging_level_invalid",
                &[("value", &self.logging.level)],
            ));
        }

        if !["active_window", "main"].contains(&self.windows.overlay_position.as_str()) {
            errs.push(tf(
                "errors.windows_overlay_position_invalid",
                &[("value", &self.windows.overlay_position)],
            ));
        }
        if !["hover", "click"].contains(&self.windows.activation_mode.as_str()) {
            errs.push(tf(
                "errors.windows_activation_mode_invalid",
                &[("value", &self.windows.activation_mode)],
            ));
        }

        if !(1..=100).contains(&self.clipboard.max_entries) {
            errs.push(tf(
                "errors.clipboard_max_entries_invalid",
                &[("value", &self.clipboard.max_entries.to_string())],
            ));
        }
        if self.clipboard.auto_expire_days > 7 {
            errs.push(tf(
                "errors.clipboard_auto_expire_days_invalid",
                &[("value", &self.clipboard.auto_expire_days.to_string())],
            ));
        }
        if !["mouse", "main"].contains(&self.clipboard.picker_position.as_str()) {
            errs.push(tf(
                "errors.clipboard_picker_position_invalid",
                &[("value", &self.clipboard.picker_position)],
            ));
        }

        for (i, p) in self.mouse.profiles.iter().enumerate() {
            let prefix = format!("mouse.profiles[{i}]");
            if let Some(ref mode) = p.scroll_mode {
                if !["default", "line"].contains(&mode.as_str()) {
                    errs.push(tf("errors.mouse_scroll_mode_invalid", &[("value", mode)]));
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
            // Pointer acceleration / tracking speed: 0..=10 (see MOUSE_ACCELERATION_MAX).
            if let Some(acc) = p.pointer.as_ref().and_then(|ptr| ptr.acceleration) {
                if !(MOUSE_ACCELERATION_MIN..=MOUSE_ACCELERATION_MAX).contains(&acc) {
                    let msg = tf(
                        "errors.mouse_pointer_acceleration_invalid",
                        &[("value", &acc.to_string())],
                    );
                    errs.push(format!("{prefix}.pointer.acceleration: {msg}"));
                }
            }
            // Button mappings: valid button numbers (numeric, >= 2) + parseable shortcuts.
            errs.extend(crate::mouse::shortcut::validate_mappings(
                &p.button_mappings,
                &prefix,
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
            // Booleans are always valid; adopt the loaded value unconditionally
            // (same convention as the other boolean switches).
            self.layout.thumbnails_enabled = other.layout.thumbnails_enabled;
            self.layout.focused_thumbnail_prewarm = other.layout.focused_thumbnail_prewarm;
            self.layout.show_app_name_in_cards = other.layout.show_app_name_in_cards;
            if !errs.iter().any(|e| e.starts_with("layout.card_text_size")) {
                self.layout.card_text_size = other.layout.card_text_size;
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
                .any(|e| e.starts_with("windows.show_hidden_app_windows"))
            {
                self.windows.show_hidden_app_windows = other.windows.show_hidden_app_windows;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("windows.overlay_position"))
            {
                self.windows.overlay_position = other.windows.overlay_position;
            }
            if !errs
                .iter()
                .any(|e| e.starts_with("windows.activation_mode"))
            {
                self.windows.activation_mode = other.windows.activation_mode;
            }
        }

        // logging
        if !has_error("logging.") {
            self.logging = other.logging;
        } else {
            if !errs.iter().any(|e| e.starts_with("logging.level")) {
                self.logging.level = other.logging.level;
            }
            // file_path has no validation, always valid
            self.logging.file_path = other.logging.file_path;
        }

        // startup (bool field needs no validation, always valid)
        self.startup = other.startup;

        // updates (bool field needs no validation; Sparkle applies it at runtime).
        self.updates = other.updates;

        // clipboard (enabled always valid; max_entries is validated)
        self.clipboard.enabled = other.clipboard.enabled;
        // show_source_app / persist / move_used_to_top / delete_after_paste /
        // show_source_app / persist / move_used_to_top / delete_after_paste /
        // clear_system_pasteboard_after_paste are bools, always valid.
        self.clipboard.show_source_app = other.clipboard.show_source_app;
        self.clipboard.persist = other.clipboard.persist;
        self.clipboard.move_used_to_top = other.clipboard.move_used_to_top;
        self.clipboard.delete_after_paste = other.clipboard.delete_after_paste;
        self.clipboard.clear_system_pasteboard_after_paste =
            other.clipboard.clear_system_pasteboard_after_paste;
        if !errs.iter().any(|e| e.starts_with("clipboard.max_entries")) {
            self.clipboard.max_entries = other.clipboard.max_entries;
        }
        if !errs
            .iter()
            .any(|e| e.starts_with("clipboard.auto_expire_days"))
        {
            self.clipboard.auto_expire_days = other.clipboard.auto_expire_days;
        }
        if !errs
            .iter()
            .any(|e| e.starts_with("clipboard.picker_position"))
        {
            self.clipboard.picker_position = other.clipboard.picker_position.clone();
        }
        // pin_follow_selection is a bool, always valid.
        self.clipboard.pin_follow_selection = other.clipboard.pin_follow_selection;

        // mouse: per-profile, per-field merge (continuing the per-field resilient pattern).
        // enabled and bool fields are always valid; each profile's fields are kept or dropped
        // based on per-field validation results.
        self.mouse.enabled = other.mouse.enabled;
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
            // Button mappings: per-entry validation; only valid entries survive.
            for (btn, desc) in &p.button_mappings {
                let ep = format!("{prefix}.button_mappings[{btn}]");
                if !errs.iter().any(|e| e.starts_with(&ep)) {
                    merged_p.button_mappings.insert(btn.clone(), desc.clone());
                }
            }
            // pointer.disable_acceleration is a bool (always valid); acceleration must pass
            // the range check.
            let accel_ok = !errs
                .iter()
                .any(|e| e.starts_with(&format!("{prefix}.pointer.acceleration")));
            merged_p.pointer = p.pointer.clone().map(|mut ptr| {
                if !accel_ok {
                    ptr.acceleration = None;
                }
                ptr
            });
            self.mouse.profiles.push(merged_p);
        }
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

/// Return a TOML value containing the expected type shape for every configuration field.
///
/// `serde(default)` handles missing fields, but Serde rejects a whole struct when one field
/// has the wrong TOML type. The schema below lets the loader inspect fields independently.
/// Optional fields are populated only in the schema (not in the fallback value), so a type
/// error still falls back to `None` where that is the runtime meaning of an omitted field.
fn config_type_schema() -> toml::Value {
    let mut schema = Config::default();
    for profile in &mut schema.mouse.profiles {
        profile.device.vendor_id = Some(0);
        profile.device.product_id = Some(0);
        profile.button_mappings_enabled = Some(true);
    }
    schema.mouse.reverse_scroll = Some(false);
    schema.mouse.scroll_mode = Some("default".to_string());
    schema.mouse.line_count = Some(3);
    schema.mouse.pointer = Some(PointerSection::default());

    let mut value = toml::to_string(&schema)
        .expect("default config must serialize")
        .parse::<toml::Value>()
        .expect("serialized default config must parse as TOML");

    // Legacy mouse fields are intentionally skipped during serialization, but they are still
    // accepted while reading old files and therefore need to be present in the type schema.
    if let Some(mouse) = value
        .as_table_mut()
        .and_then(|root| root.get_mut("mouse"))
        .and_then(toml::Value::as_table_mut)
    {
        mouse.insert("reverse_scroll".into(), toml::Value::Boolean(false));
        mouse.insert("scroll_mode".into(), toml::Value::String("default".into()));
        mouse.insert("line_count".into(), toml::Value::Integer(3));
        mouse.insert(
            "pointer".into(),
            toml::Value::Table(
                [("disable_acceleration".into(), toml::Value::Boolean(false))]
                    .into_iter()
                    .collect(),
            ),
        );
    }
    value
}

fn toml_value_type(value: &toml::Value) -> &'static str {
    match value {
        toml::Value::String(_) => "string",
        toml::Value::Integer(_) => "integer",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "boolean",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
    }
}

fn toml_path_child(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn toml_path_index(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn type_error(path: &str, actual: &toml::Value, expected: &toml::Value) -> String {
    let field = if path.is_empty() { "<root>" } else { path };
    tf(
        "errors.config_field_type_invalid",
        &[
            ("field", field),
            ("actual", toml_value_type(actual)),
            ("expected", toml_value_type(expected)),
        ],
    )
}

/// Sanitize one TOML value against the expected type shape while preserving valid siblings.
///
/// The return value is `None` when a malformed optional field should be omitted so Serde can
/// apply its normal default. Unknown fields remain untouched and are ignored by Serde.
fn sanitize_config_value(
    actual: &toml::Value,
    fallback: Option<&toml::Value>,
    schema: &toml::Value,
    path: &str,
    errors: &mut Vec<String>,
) -> Option<toml::Value> {
    // `button_mappings` is a dynamic string map, so an empty default table cannot describe its
    // value type. Validate each value explicitly instead of adding a fake schema key.
    if path.ends_with(".button_mappings") {
        let Some(table) = actual.as_table() else {
            errors.push(type_error(path, actual, schema));
            return fallback.cloned();
        };
        let mut sanitized = table.clone();
        for (key, value) in table {
            if !value.is_str() {
                let entry_path = format!("{path}[{key}]");
                let expected = toml::Value::String(String::new());
                errors.push(type_error(&entry_path, value, &expected));
                sanitized.remove(key);
            }
        }
        return Some(toml::Value::Table(sanitized));
    }

    match (actual, schema) {
        (toml::Value::Table(actual_table), toml::Value::Table(schema_table)) => {
            let fallback_table = fallback.and_then(toml::Value::as_table);
            let mut sanitized = actual_table.clone();
            for (key, schema_child) in schema_table {
                let Some(actual_child) = actual_table.get(key) else {
                    continue;
                };
                let child_path = toml_path_child(path, key);
                let fallback_child = fallback_table.and_then(|table| table.get(key));
                match sanitize_config_value(
                    actual_child,
                    fallback_child,
                    schema_child,
                    &child_path,
                    errors,
                ) {
                    Some(value) => {
                        sanitized.insert(key.clone(), value);
                    }
                    None => {
                        sanitized.remove(key);
                    }
                }
            }
            Some(toml::Value::Table(sanitized))
        }
        (toml::Value::Array(actual_array), toml::Value::Array(schema_array)) => {
            let Some(schema_item) = schema_array.first() else {
                return Some(toml::Value::Array(actual_array.clone()));
            };
            let fallback_array = fallback.and_then(toml::Value::as_array);
            let mut sanitized = Vec::with_capacity(actual_array.len());
            for (index, actual_item) in actual_array.iter().enumerate() {
                let item_path = toml_path_index(path, index);
                let item_fallback = if path == "mouse.profiles" && index > 0 {
                    // Only the first profile is the explicit wildcard default. A malformed
                    // optional field in a per-device profile must fall back to omission, not
                    // accidentally inherit the wildcard profile's concrete value.
                    None
                } else {
                    fallback_array.and_then(|items| items.first())
                };
                if let Some(value) = sanitize_config_value(
                    actual_item,
                    item_fallback,
                    schema_item,
                    &item_path,
                    errors,
                ) {
                    sanitized.push(value);
                }
            }
            Some(toml::Value::Array(sanitized))
        }
        (toml::Value::String(_), toml::Value::String(_))
        | (toml::Value::Integer(_), toml::Value::Integer(_))
        | (toml::Value::Boolean(_), toml::Value::Boolean(_))
        | (toml::Value::Datetime(_), toml::Value::Datetime(_))
        | (toml::Value::Float(_), toml::Value::Float(_))
        // TOML integers are accepted by Serde when loading an f64 field.
        | (toml::Value::Integer(_), toml::Value::Float(_)) => Some(actual.clone()),
        _ => {
            errors.push(type_error(path, actual, schema));
            fallback.cloned()
        }
    }
}

/// Parse and sanitize one config file. The boolean says whether the repaired/migrated value
/// should be persisted back to disk; TOML syntax or whole-document deserialization failures
/// return Err so the original file remains available for diagnosis.
fn parse_config_content(content: &str) -> Result<(Config, Vec<String>, bool), Vec<String>> {
    let actual = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(error) => {
            return Err(vec![tf(
                "errors.config_read_failed",
                &[("error", &error.to_string())],
            )]);
        }
    };
    let defaults = toml::to_string(&Config::default())
        .expect("default config must serialize")
        .parse::<toml::Value>()
        .expect("serialized default config must parse as TOML");
    let schema = config_type_schema();
    let mut errors = Vec::new();
    let sanitized = sanitize_config_value(&actual, Some(&defaults), &schema, "", &mut errors)
        .unwrap_or(defaults.clone());
    let mut loaded: Config = match sanitized.try_into() {
        Ok(config) => config,
        Err(error) => {
            // This is a defensive guard for a schema mismatch introduced by a future field.
            // The normal path above should make every field independently deserializable.
            errors.push(tf(
                "errors.config_read_failed",
                &[("error", &error.to_string())],
            ));
            return Err(errors);
        }
    };

    let mut needs_persist = !errors.is_empty();
    needs_persist |= loaded.mouse.migrate_legacy();
    needs_persist |= loaded.migrate_card_text_style();
    errors.extend(loaded.validate());
    if !errors.is_empty() {
        let mut merged = Config::default();
        merged.merge_valid(loaded, &errors);
        Ok((merged, errors, true))
    } else {
        Ok((loaded, errors, needs_persist))
    }
}

fn config_path() -> std::path::PathBuf {
    config_path_in(&std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()))
}

/// Whether the config file already exists. The guide uses it to tell a fresh install from an
/// upgrade (which must not be nagged).
pub(crate) fn config_file_exists() -> bool {
    config_path().exists()
}

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

static ATOMIC_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write through a same-directory temp file, fsync, and atomically rename it into place so a
/// reader never observes a partial file and stale writers cannot truncate a newer file.
fn atomic_write(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("config path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("create config directory {}: {}", parent.display(), e))?;

    let serial = ATOMIC_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("config"),
        std::process::id(),
        serial
    ));
    let result = (|| {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| format!("create {}: {}", temp.display(), e))?;
        file.write_all(contents)
            .map_err(|e| format!("write {}: {}", temp.display(), e))?;
        file.sync_all()
            .map_err(|e| format!("sync {}: {}", temp.display(), e))?;
        drop(file);
        std::fs::rename(&temp, path).map_err(|e| format!("replace {}: {}", path.display(), e))?;
        // Best effort directory sync: the rename is durable on filesystems that support it.
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

impl Config {
    /// Serialize to a given path (pure logic; tests inject a temp dir).
    fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let toml_str =
            toml::to_string_pretty(self).map_err(|e| format!("serialize config: {}", e))?;
        atomic_write(path, toml_str.as_bytes())
    }

    pub fn load_or_default() -> (Self, Vec<String>) {
        let path = config_path();
        Self::load_or_default_from(&path)
    }

    /// One-time migration for the card text style: after the content swap (window title
    /// as primary line, app name as secondary), the OLD default combo (title 11/0.23 +
    /// app_name 13/0.5, plus the old color pairs) inverts the hierarchy under the new
    /// layout (the secondary line would render larger than the primary). Rewrites only
    /// when ALL four font values exactly match the old defaults; colors likewise rewrite
    /// only on an exact PAIR match against the old dark or old light defaults -- any
    /// customized value skips the whole rewrite (conservative migration, never clobbers
    /// customizations). Idempotent: the new defaults no longer match the old values.
    /// Returns whether anything changed.
    pub(crate) fn migrate_card_text_style(&mut self) -> bool {
        let mut changed = false;
        // Old font defaults: title_size 11 / weight 0.23, app_name_size 13 / weight 0.5.
        let f = &mut self.fonts;
        if f.title_size == 11.0
            && f.title_weight == 0.23
            && f.app_name_size == 13.0
            && f.app_name_weight == 0.5
        {
            f.title_size = 12.0; // reference: title 12px medium
            f.app_name_size = 10.0; // app name 10px regular
            f.app_name_weight = 0.0;
            changed = true;
        }
        // Colors migrate per theme section: each section's pair independently and
        // exactly matches the old dark (dddddd/888888) or old light (1a1a1a/333333)
        // defaults before being rewritten to its new same-family pair (D1/57 alpha).
        for theme in [&mut self.colors.dark, &mut self.colors.light] {
            let pair = (theme.app_name.as_str(), theme.win_title.as_str());
            match pair {
                ("ddddddff", "888888ff") => {
                    theme.app_name = "FFFFFF57".into();
                    theme.win_title = "FFFFFFD1".into();
                    changed = true;
                }
                ("1a1a1aff", "333333ff") => {
                    theme.app_name = "00000057".into();
                    theme.win_title = "000000D1".into();
                    changed = true;
                }
                _ => {}
            }
        }
        changed
    }

    /// Load from a given path (pure logic; tests inject a temp dir).
    fn load_or_default_from(path: &std::path::Path) -> (Self, Vec<String>) {
        Self::load_or_default_from_result(path, std::fs::read_to_string(path))
    }

    fn load_or_default_from_result(
        path: &std::path::Path,
        read_result: std::io::Result<String>,
    ) -> (Self, Vec<String>) {
        match read_result {
            Ok(content) => match parse_config_content(&content) {
                Ok((loaded, errs, needs_persist)) => {
                    if needs_persist {
                        let _ = loaded.save_to(path);
                    }
                    (loaded, errs)
                }
                Err(errs) => (Config::default(), errs),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Write defaults only when the file is genuinely missing; preserve it on all
                // other read errors.
                let defaults = Config::default();
                let _ = defaults.save_to(path);
                (defaults, Vec::new())
            }
            Err(error) => (
                Config::default(),
                vec![tf(
                    "errors.config_read_failed",
                    &[("error", &error.to_string())],
                )],
            ),
        }
    }

    pub fn reload() -> Result<(Self, Vec<String>, bool), Vec<String>> {
        let path = config_path();
        Self::reload_from(&path)
    }

    fn reload_from(path: &std::path::Path) -> Result<(Self, Vec<String>, bool), Vec<String>> {
        let content = std::fs::read_to_string(path).map_err(|error| {
            vec![tf(
                "errors.config_read_failed",
                &[("error", &error.to_string())],
            )]
        })?;
        parse_config_content(&content)
    }
}

pub static CONFIG: std::sync::LazyLock<RwLock<Config>> = std::sync::LazyLock::new(|| {
    let (cfg, _errs) = Config::load_or_default();
    // Apply the locale from config (I18N init only used the system locale).
    // No cycle: I18N does not read CONFIG; see the note at the top of i18n.rs.
    i18n::apply_config_locale(&cfg.i18n.locale);
    RwLock::new(cfg)
});

/// Return the effective glass style.
pub fn effective_glass_style() -> String {
    CONFIG.read().unwrap().appearance.glass_style.clone()
}

/// Return the effective glass tint.
pub fn effective_glass_tint() -> String {
    CONFIG.read().unwrap().appearance.glass_tint.clone()
}

/// Return whether focused-window thumbnail prewarming is enabled.
pub(crate) fn focused_thumbnail_prewarm_enabled() -> bool {
    CONFIG.read().unwrap().layout.focused_thumbnail_prewarm
}

/// Persistence debounce window: bursts of changes (slider drags, fast typing) coalesce into
/// one disk write after this quiet period.
const PERSIST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(400);

enum PersistMsg {
    Schedule {
        snapshot: Config,
        revision: u64,
    },
    Flush {
        snapshot: Config,
        revision: u64,
        ack: Option<mpsc::Sender<Result<(), String>>>,
    },
}

static CONFIG_REVISION: AtomicU64 = AtomicU64::new(0);

/// The background persistence thread's channel (lazy; control callbacks and restores send).
static PERSIST_TX: LazySender = LazySender::new();

/// A lazy wrapper for the mpsc Sender (the thread spawns with the channel).
struct LazySender(std::sync::Mutex<Option<std::sync::mpsc::Sender<PersistMsg>>>);

impl LazySender {
    const fn new() -> Self {
        Self(std::sync::Mutex::new(None))
    }

    fn send(&self, msg: PersistMsg) -> Result<(), String> {
        let mut guard = self.0.lock().unwrap();
        if guard.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::Builder::new()
                .name("config-persist".into())
                .spawn(move || persist_thread(rx))
                .expect("spawn config-persist thread");
            *guard = Some(tx);
        }
        // A send failure only means the thread exited (receiver dropped); dropping the
        // message is safer than panicking.
        guard
            .as_ref()
            .unwrap()
            .send(msg)
            .map_err(|_| "config persistence writer stopped".to_string())
    }
}

/// The persistence thread: Schedule opens a debounce window (write once after 400ms of
/// quiet); Flush writes immediately. It always snapshots the newest CONFIG, so bursts
/// naturally coalesce into a single write.
fn persist_thread(rx: std::sync::mpsc::Receiver<PersistMsg>) {
    let mut persisted_revision = 0;
    while let Ok(msg) = rx.recv() {
        match msg {
            PersistMsg::Flush {
                snapshot,
                revision,
                ack,
            } => {
                let result = persist_if_newer(snapshot, revision, &mut persisted_revision);
                if let Some(ack) = ack {
                    let _ = ack.send(result);
                }
            }
            PersistMsg::Schedule { snapshot, revision } => {
                let mut latest = (snapshot, revision);
                loop {
                    match rx.recv_timeout(PERSIST_DEBOUNCE) {
                        Ok(PersistMsg::Schedule { snapshot, revision }) => {
                            if revision > latest.1 {
                                latest = (snapshot, revision);
                            }
                        }
                        Ok(PersistMsg::Flush {
                            snapshot,
                            revision,
                            ack,
                        }) => {
                            if revision > latest.1 {
                                latest = (snapshot, revision);
                            }
                            let result = persist_if_newer(
                                latest.0.clone(),
                                latest.1,
                                &mut persisted_revision,
                            );
                            if let Some(ack) = ack {
                                let _ = ack.send(result);
                            }
                            break;
                        }
                        // Timeout = quiet period elapsed; write once.
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let _ = persist_if_newer(
                                latest.0.clone(),
                                latest.1,
                                &mut persisted_revision,
                            );
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        }
    }
}

fn persist_if_newer(
    snapshot: Config,
    revision: u64,
    persisted_revision: &mut u64,
) -> Result<(), String> {
    if revision <= *persisted_revision {
        return Ok(());
    }
    let result = snapshot.save_to(&config_path());
    if result.is_ok() {
        *persisted_revision = revision;
    }
    result
}

fn snapshot_with_revision() -> (Config, u64) {
    let snapshot = CONFIG.read().unwrap().clone();
    let revision = CONFIG_REVISION.fetch_add(1, Ordering::SeqCst) + 1;
    (snapshot, revision)
}

/// Call after a setting change: schedule one debounced disk write (immediate effect and disk
/// persistence are decoupled to avoid burst IO while dragging/typing).
pub fn schedule_config_persist() {
    let (snapshot, revision) = snapshot_with_revision();
    if let Err(e) = PERSIST_TX.send(PersistMsg::Schedule { snapshot, revision }) {
        eprintln!("[config] persist schedule failed: {}", e);
    }
}

/// Queue an immediate write request for the writer (restore-default / blur-commit paths).
pub fn persist_config_now() {
    let (snapshot, revision) = snapshot_with_revision();
    if let Err(e) = PERSIST_TX.send(PersistMsg::Flush {
        snapshot,
        revision,
        ack: None,
    }) {
        eprintln!("[config] persist request failed: {}", e);
    }
}

/// Synchronously wait until the newest configuration snapshot is durable, used before quitting.
pub fn flush_config_sync() -> Result<(), String> {
    let (snapshot, revision) = snapshot_with_revision();
    let (ack_tx, ack_rx) = mpsc::channel();
    PERSIST_TX.send(PersistMsg::Flush {
        snapshot,
        revision,
        ack: Some(ack_tx),
    })?;
    ack_rx
        .recv()
        .map_err(|_| "config persistence writer stopped".to_string())?
}

/// Reload config from disk and apply. Returns validation errors (empty = success).
pub fn reload_config() -> Vec<String> {
    let (new_cfg, errs, needs_persist) = match Config::reload() {
        Ok(result) => result,
        Err(errs) => return errs,
    };
    let old_cfg = CONFIG.read().unwrap().clone();
    if let Ok(mut cfg) = CONFIG.write() {
        *cfg = new_cfg.clone();
    }
    crate::runtime_config::apply_config_change(
        &old_cfg,
        &new_cfg,
        crate::runtime_config::ConfigChangeSource::Reload,
    );
    if needs_persist {
        // Reload callbacks never write synchronously; repaired/migrated snapshots go through
        // the single writer.
        persist_config_now();
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex8_parses_valid_rgb_alpha() {
        assert_eq!(parse_hex8("999999ff"), 0x999999ff);
        assert_eq!(parse_hex8("00000000"), 0x00000000);
        assert_eq!(parse_hex8("FFFFFFFF"), 0xffffffff);
        assert_eq!(parse_hex8("12345678"), 0x12345678);
    }

    #[test]
    fn parse_hex8_invalid_inputs_fall_back_to_zero() {
        // Invalid/empty/overlong inputs all fall back to 0, never panic.
        assert_eq!(parse_hex8(""), 0);
        assert_eq!(parse_hex8("xyz"), 0);
        assert_eq!(parse_hex8("999999999"), 0); // 9 chars overflows u32
        assert_eq!(parse_hex8("gggggggg"), 0);
    }

    fn assert_err_count(cfg: &Config, expected: usize) {
        let errs = cfg.validate();
        assert_eq!(errs.len(), expected, "errors: {:?}", errs);
    }

    #[test]
    fn defaults_validate_clean() {
        let cfg = Config::default();
        assert_err_count(&cfg, 0);
        assert!(!cfg.windows.show_minimized);
        assert!(!cfg.windows.show_hidden_app_windows);
    }

    #[test]
    fn window_control_direction_defaults_preserve_existing_configs() {
        let cfg: Config = toml::from_str("[window_control]\nenabled = true\n").unwrap();
        assert!(cfg.window_control.enabled);
        assert!(cfg.window_control.up);
        assert!(cfg.window_control.down);
        assert!(cfg.window_control.left);
        assert!(cfg.window_control.right);
        assert!(cfg.window_control.display_up);
        assert!(cfg.window_control.display_down);
        assert!(cfg.window_control.display_left);
        assert!(cfg.window_control.display_right);
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
    fn retired_icon_layout_fields_are_ignored() {
        // Retired experimental layout fields are ignored when loading and do not cause errors.
        let cfg: Config = toml::from_str(
            "[layout]\ncards_per_row = 10\ncard_width = 300.0\ncard_height = 400.0\ncard_gap = 8.0\nicon_size = 200.0\n",
        )
        .unwrap();
        assert!(cfg.validate().is_empty());
        assert!(cfg.layout.thumbnails_enabled);
    }

    #[test]
    fn validate_catches_keyboard_and_locale() {
        let mut cfg = Config::default();
        cfg.keyboard.modifier = "ctrl".into();
        cfg.i18n.locale = "fr".into();
        cfg.logging.level = "verbose".into();
        cfg.windows.overlay_position = "nowhere".into();
        cfg.windows.activation_mode = "double_click".into();
        assert_err_count(&cfg, 5);
    }

    #[test]
    fn validate_rejects_card_text_size_outside_range() {
        let mut cfg = Config::default();
        cfg.layout.card_text_size = 12.9;
        assert_err_count(&cfg, 1);
        cfg.layout.card_text_size = 20.1;
        assert_err_count(&cfg, 1);
        cfg.layout.card_text_size = 13.0;
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn validate_rejects_status_bar_text_size_outside_range() {
        let mut cfg = Config::default();
        cfg.fonts.status_bar_size = 12.9;
        assert_err_count(&cfg, 1);
        cfg.fonts.status_bar_size = 20.1;
        assert_err_count(&cfg, 1);
        cfg.fonts.status_bar_size = 13.0;
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn switcher_text_defaults_to_fifteen_points() {
        let cfg = Config::default();
        assert_eq!(cfg.layout.card_text_size, 15.0);
        assert_eq!(cfg.fonts.status_bar_size, 15.0);
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

    #[test]
    fn merge_valid_keeps_healthy_sections_wholesale() {
        // A fully valid config is merged wholesale with all custom values kept (the old test
        // merged defaults into defaults, making the assertions vacuous).
        let mut other = Config::default();
        other.appearance.theme = "dark".into();
        other.keyboard.modifier = "option".into();
        other.i18n.locale = "zh-Hant".into();
        other.mouse.enabled = true;
        other.clipboard.enabled = true;
        other.clipboard.max_entries = 30;
        other.clipboard.persist = true;
        other.clipboard.move_used_to_top = false;
        other.clipboard.delete_after_paste = true;
        other.clipboard.clear_system_pasteboard_after_paste = true;
        other.clipboard.pin_follow_selection = false;
        other.layout.card_text_size = 16.0;
        let mut merged = Config::default();
        merged.merge_valid(other, &[]);
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(merged.keyboard.modifier, "option");
        assert_eq!(merged.i18n.locale, "zh-Hant");
        assert!(merged.mouse.enabled);
        assert!(merged.clipboard.enabled);
        assert_eq!(merged.clipboard.max_entries, 30);
        assert!(merged.clipboard.persist);
        assert!(!merged.clipboard.move_used_to_top);
        assert!(merged.clipboard.delete_after_paste);
        assert!(merged.clipboard.clear_system_pasteboard_after_paste);
        assert!(!merged.clipboard.pin_follow_selection);
        assert_eq!(merged.layout.card_text_size, 16.0);
    }

    #[test]
    fn validate_rejects_out_of_range_clipboard_max_entries() {
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
    fn retired_clipboard_highlight_limits_are_ignored() {
        // Highlight limits in old configs are retired, but serde must ignore them so upgrades do
        // not make the entire config fail to load.
        let cfg: Config = toml::from_str(
            "[clipboard]\nmax_highlight_bytes = 65536\nmax_highlight_lines = 1000\n",
        )
        .unwrap();
        assert_eq!(cfg.clipboard.max_entries, 50);
    }

    #[test]
    fn validate_rejects_unknown_clipboard_picker_position() {
        // The picker position only accepts mouse / main.
        let mut cfg = Config::default();
        assert_eq!(cfg.clipboard.picker_position, "main");
        cfg.clipboard.picker_position = "top-right".into();
        assert_err_count(&cfg, 1);
        cfg.clipboard.picker_position = "mouse".into();
        assert_err_count(&cfg, 0);
        cfg.clipboard.picker_position = "main".into();
        assert_err_count(&cfg, 0);
    }

    #[test]
    fn validate_accepts_auto_expire_days_0_to_7() {
        // Auto-expire days: 0 (never) and 7 are valid, 8 is rejected.
        let mut cfg = Config::default();
        assert_eq!(cfg.clipboard.auto_expire_days, 3, "default is 3 days");
        assert_err_count(&cfg, 0);
        cfg.clipboard.auto_expire_days = 0;
        assert_err_count(&cfg, 0);
        cfg.clipboard.auto_expire_days = 7;
        assert_err_count(&cfg, 0);
        cfg.clipboard.auto_expire_days = 8;
        assert_err_count(&cfg, 1);
    }

    #[test]
    fn merge_valid_resets_only_invalid_fields() {
        let mut other = Config::default();
        // Valid field: everything customized.
        other.appearance.theme = "dark".into();
        other.appearance.glass_style = "regular".into();
        other.appearance.glass_tint = "11223344".into();
        other.appearance.corner_radius = 12.0;
        // Invalid field: corner_radius < 0.
        let mut cfg = other.clone();
        cfg.appearance.corner_radius = -5.0;
        let errs = cfg.validate();
        assert_eq!(errs.len(), 1);

        let mut merged = Config::default();
        merged.merge_valid(cfg, &errs);
        // Valid fields survive; the invalid one falls back to the default.
        assert_eq!(merged.appearance.theme, "dark");
        assert_eq!(merged.appearance.glass_tint, "11223344");
        assert_eq!(
            merged.appearance.corner_radius,
            Config::default().appearance.corner_radius
        );
    }

    #[test]
    fn migrate_legacy_is_idempotent() {
        let mut cfg = Config::default();
        let before = cfg.mouse.clone();
        cfg.mouse.migrate_legacy();
        cfg.mouse.migrate_legacy();
        // A second migration must not change anything.
        assert_eq!(cfg.mouse.profiles.len(), before.profiles.len());
        assert!(cfg.mouse.reverse_scroll.is_none());
    }

    #[test]
    fn card_text_style_migration_rewrites_old_defaults() {
        let mut cfg = Config::default();
        // Inject the old-default combo (pre-swap fonts and colors): fonts + the old dark
        // pair in the dark section + the old light pair in the light section.
        cfg.fonts.title_size = 11.0;
        cfg.fonts.title_weight = 0.23;
        cfg.fonts.app_name_size = 13.0;
        cfg.fonts.app_name_weight = 0.5;
        cfg.colors.dark.app_name = "ddddddff".into();
        cfg.colors.dark.win_title = "888888ff".into();
        cfg.colors.light.app_name = "1a1a1aff".into();
        cfg.colors.light.win_title = "333333ff".into();

        assert!(cfg.migrate_card_text_style());
        assert_eq!(cfg.fonts.title_size, 12.0);
        assert_eq!(cfg.fonts.title_weight, 0.23);
        assert_eq!(cfg.fonts.app_name_size, 10.0);
        assert_eq!(cfg.fonts.app_name_weight, 0.0);
        // each section migrates to its same-family new pair.
        assert_eq!(cfg.colors.dark.app_name, "FFFFFF57");
        assert_eq!(cfg.colors.dark.win_title, "FFFFFFD1");
        assert_eq!(cfg.colors.light.app_name, "00000057");
        assert_eq!(cfg.colors.light.win_title, "000000D1");
        // Idempotent: the new defaults no longer match the old values; a second call is a no-op.
        assert!(!cfg.migrate_card_text_style());
    }

    #[test]
    fn card_text_style_migration_skips_customized_values() {
        let mut cfg = Config::default();
        // A single customized font value -> the whole group is skipped (conservative;
        // colors are at the new defaults here and do not trigger either).
        cfg.fonts.title_size = 11.0;
        cfg.fonts.title_weight = 0.23;
        cfg.fonts.app_name_size = 13.0;
        cfg.fonts.app_name_weight = 0.4; // customized
        assert!(!cfg.migrate_card_text_style());
        assert_eq!(cfg.fonts.app_name_weight, 0.4);

        // Colors likewise: with NEITHER section pairing up against an old default, nothing moves.
        cfg.colors.dark.app_name = "custom01ff".into();
        cfg.colors.light.app_name = "custom01ff".into();
        assert!(!cfg.migrate_card_text_style());
        assert_eq!(cfg.colors.dark.app_name, "custom01ff");
        assert_eq!(cfg.colors.light.app_name, "custom01ff");
    }

    #[test]
    fn card_text_style_migration_handles_dark_color_pair() {
        let mut cfg = Config::default();
        cfg.fonts.title_size = 12.0; // fonts already new
        cfg.colors.dark.app_name = "ddddddff".into();
        cfg.colors.dark.win_title = "888888ff".into();
        assert!(cfg.migrate_card_text_style());
        assert_eq!(cfg.colors.dark.app_name, "FFFFFF57");
        assert_eq!(cfg.colors.dark.win_title, "FFFFFFD1");
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

    #[test]
    fn save_and_load_roundtrip_preserves_custom_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut cfg = Config::default();
        // All fields set to non-default values -- the old default-value roundtrip masked the
        // migrate_legacy overwrite bug (see docs/test-review.md); this is the regression guard.
        cfg.appearance.theme = "dark".into();
        cfg.appearance.glass_tint = "11223344".into();
        cfg.keyboard.modifier = "option".into();
        cfg.i18n.locale = "zh-Hans".into();
        cfg.updates.automatically_check = false;
        cfg.windows.overlay_position = "main".into();
        cfg.windows.activation_mode = "click".into();
        cfg.windows.show_minimized = true;
        cfg.windows.show_hidden_app_windows = false;
        cfg.windows.enabled = false; // non-default: verify the roundtrip
        cfg.mouse.enabled = true;
        cfg.mouse.profiles = vec![
            MouseProfile {
                reverse_scroll: Some(true),
                scroll_mode: Some("line".into()),
                line_count: Some(5),
                pointer: Some(PartialPointerSection {
                    disable_acceleration: Some(true),
                    acceleration: Some(1.25),
                }),
                ..Default::default()
            },
            MouseProfile {
                device: DeviceMatcher {
                    vendor_id: Some(1133),
                    product_id: Some(17492),
                    ..Default::default()
                },
                reverse_scroll: Some(false),
                ..Default::default()
            },
        ];
        cfg.save_to(&path).unwrap();

        let (loaded, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        // Non-mouse fields survive the roundtrip.
        assert_eq!(loaded.appearance.theme, "dark");
        assert_eq!(loaded.appearance.glass_tint, "11223344");
        assert_eq!(loaded.keyboard.modifier, "option");
        assert_eq!(loaded.i18n.locale, "zh-Hans");
        assert!(!loaded.updates.automatically_check);
        assert_eq!(loaded.windows.overlay_position, "main");
        assert_eq!(loaded.windows.activation_mode, "click");
        assert!(loaded.windows.show_minimized);
        assert!(!loaded.windows.show_hidden_app_windows);
        assert!(!loaded.windows.enabled);
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
        assert_eq!(w.pointer.as_ref().and_then(|x| x.acceleration), Some(1.25));
        let d = &loaded.mouse.profiles[1];
        assert_eq!(d.device.vendor_id, Some(1133));
        assert_eq!(d.device.product_id, Some(17492));
        assert_eq!(d.reverse_scroll, Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_sets_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Config::default().save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("[appearance]"));
    }

    #[test]
    fn load_new_format_config_leaves_profiles_untouched() {
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
        // Wildcard profile: custom values preserved.
        let w = &cfg.mouse.profiles[0];
        assert_eq!(w.reverse_scroll, Some(true));
        assert_eq!(w.scroll_mode.as_deref(), Some("line"));
        assert_eq!(w.line_count, Some(5));
        assert_eq!(
            w.pointer.as_ref().and_then(|x| x.disable_acceleration),
            Some(true)
        );
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
        // Missing file: defaults written and returned, no errors.
        assert!(errs.is_empty());
        assert_eq!(cfg.appearance.theme, "auto");
        assert!(path.exists());
    }

    #[test]
    fn load_read_error_keeps_existing_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "custom_setting = 'keep me'";
        std::fs::write(&path, original).unwrap();

        let (cfg, errs) = Config::load_or_default_from_result(
            &path,
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            )),
        );

        assert_eq!(cfg.appearance.theme, Config::default().appearance.theme);
        assert!(!errs.is_empty());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn reload_fatal_errors_leave_the_file_untouched_and_missing_files_missing() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_path = dir.path().join("invalid.toml");
        let original = "[appearance\ntheme = 'dark'";
        std::fs::write(&invalid_path, original).unwrap();

        assert!(Config::reload_from(&invalid_path).is_err());
        assert_eq!(std::fs::read_to_string(&invalid_path).unwrap(), original);

        let missing_path = dir.path().join("missing.toml");
        assert!(Config::reload_from(&missing_path).is_err());
        assert!(!missing_path.exists());
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
"#,
        )
        .unwrap();
        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(!errs.is_empty());
        // Valid fields survive; the invalid color falls back to default; retired layout fields are ignored.
        assert_eq!(cfg.appearance.theme, "dark");
        assert_eq!(
            cfg.appearance.glass_tint,
            Config::default().appearance.glass_tint
        );
    }

    #[test]
    fn load_type_error_keeps_valid_siblings_and_persists_repair() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[appearance]
theme = "dark"
corner_radius = "not-a-number"

[keyboard]
modifier = "option"
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.iter().any(|error| {
            error.contains("appearance.corner_radius") && error.contains("string")
        }));
        assert_eq!(cfg.appearance.theme, "dark");
        assert_eq!(cfg.keyboard.modifier, "option");
        assert_eq!(
            cfg.appearance.corner_radius,
            Config::default().appearance.corner_radius
        );

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("theme = \"dark\""));
        assert!(!persisted.contains("not-a-number"));
    }

    #[test]
    fn load_type_errors_are_isolated_per_mouse_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true

[[mouse.profiles]]
reverse_scroll = true
scroll_mode = 123
line_count = 5

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 17492
scroll_mode = 456
reverse_scroll = false
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[0].scroll_mode")));
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[1].scroll_mode")));
        assert!(cfg.mouse.enabled);
        assert_eq!(cfg.mouse.profiles.len(), 2);
        assert_eq!(cfg.mouse.profiles[0].reverse_scroll, Some(true));
        assert_eq!(cfg.mouse.profiles[0].line_count, Some(5));
        assert_eq!(cfg.mouse.profiles[0].scroll_mode, None);
        assert_eq!(cfg.mouse.profiles[1].device.vendor_id, Some(1133));
        assert_eq!(cfg.mouse.profiles[1].device.product_id, Some(17492));
        assert_eq!(cfg.mouse.profiles[1].reverse_scroll, Some(false));
        assert_eq!(cfg.mouse.profiles[1].scroll_mode, None);
    }

    #[test]
    fn load_out_of_range_pointer_acceleration_is_dropped_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]
enabled = true

[[mouse.profiles]]
[mouse.profiles.pointer]
disable_acceleration = true
acceleration = 2.5

[[mouse.profiles]]
device_vendor_id = 1133
device_product_id = 17492
[mouse.profiles.pointer]
acceleration = 41.0
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[1].pointer.acceleration")));
        // The valid profile keeps its value; the out-of-range one loses only acceleration,
        // while disable_acceleration still applies.
        assert_eq!(
            cfg.mouse.profiles[0].pointer.as_ref().unwrap().acceleration,
            Some(2.5)
        );
        assert_eq!(
            cfg.mouse.profiles[1].pointer.as_ref().unwrap().acceleration,
            None
        );
    }

    #[test]
    fn load_type_error_drops_only_invalid_button_mapping_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[mouse]

[[mouse.profiles]]
[mouse.profiles.button_mappings]
"2" = "cmd+v"
"3" = 42
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs
            .iter()
            .any(|error| error.contains("mouse.profiles[0].button_mappings[3]")));
        let mappings = &cfg.mouse.profiles[0].button_mappings;
        assert_eq!(mappings.get("2").map(String::as_str), Some("cmd+v"));
        assert!(!mappings.contains_key("3"));
    }

    #[test]
    fn load_syntax_error_reports_without_overwriting_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "[appearance\ntheme = \"dark\"\n";
        std::fs::write(&path, original).unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(!errs.is_empty());
        assert_eq!(cfg.appearance.theme, Config::default().appearance.theme);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn load_unknown_fields_keeps_known_values() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[appearance]
theme = "dark"
future_option = "kept for forward compatibility"
"#,
        )
        .unwrap();

        let (cfg, errs) = Config::load_or_default_from(&path);
        assert!(errs.is_empty());
        assert_eq!(cfg.appearance.theme, "dark");
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
        // Legacy fields migrated into an "All Mice" profile and persisted.
        assert_eq!(cfg.mouse.profiles.len(), 1);
        assert_eq!(cfg.mouse.profiles[0].reverse_scroll, Some(true));
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(!persisted.contains("device_vendor_id"));
        assert!(persisted.contains("reverse_scroll"));
    }

    #[test]
    fn config_path_in_uses_home_and_creates_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = config_path_in(dir.path().to_str().unwrap());
        // Path: home/.config/oh-my-tab/config.toml with the dir auto-created.
        assert_eq!(p, dir.path().join(".config/oh-my-tab/config.toml"));
        assert!(p.parent().unwrap().exists());
    }
}
