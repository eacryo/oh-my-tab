// i18n module: a handcrafted TOML-based localization system, isomorphic to config.rs.
// Zero new deps (toml/serde already present); locale files are embedded at compile time;
// the active locale is config-driven and hot-reloadable.
//
// No cycle: this module NEVER reads CONFIG, only the system locale (NSLocale). So when
// CONFIG's LazyLock init calls validate() -> t() -> I18N init, there is no deadlock.
// config.rs calls apply_config_locale() one-way after CONFIG init and after reload.

use crate::log_info;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use std::collections::HashMap;
use std::ffi::c_char;
use std::sync::{LazyLock, RwLock};

// Locale files embedded at compile time to avoid runtime file-missing / read failures.
const EN_TOML: &str = include_str!("../locales/en.toml");
const ZH_TOML: &str = include_str!("../locales/zh-Hans.toml");
const ZH_HANT_TOML: &str = include_str!("../locales/zh-Hant.toml");
const DEFAULT_LOCALE: &str = "en";
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
pub(crate) const TEST_LONG_LOCALE: &str = "__oh_my_tab_test_long_en";
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
const PSEUDO_LOCALE_FLAG: &str = "pseudo-locale";

// Supported locale -> embedded TOML text. To add a language, add a file + register here.
fn locale_raw(locale: &str) -> Option<&'static str> {
    match locale {
        "en" => Some(EN_TOML),
        "zh-Hans" => Some(ZH_TOML),
        "zh-Hant" => Some(ZH_HANT_TOML),
        _ => None,
    }
}

fn is_supported(locale: &str) -> bool {
    if locale_raw(locale).is_some() {
        return true;
    }
    #[cfg(any(debug_assertions, feature = "dev-long-text"))]
    if locale == TEST_LONG_LOCALE {
        return true;
    }
    false
}

// Flatten nested TOML tables into a "section.key" -> value map, collecting only string leaves.
fn flatten(value: &toml::Value, prefix: &str, out: &mut HashMap<String, String>) {
    if let toml::Value::Table(t) = value {
        for (k, v) in t {
            let key = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            match v {
                toml::Value::String(s) => {
                    out.insert(key, s.clone());
                }
                toml::Value::Table(_) => flatten(v, &key, out),
                _ => {} // ignore non-string leaves
            }
        }
    }
}

fn load_messages(locale: &str) -> HashMap<String, String> {
    let raw = match locale_raw(locale) {
        Some(r) => r,
        None => return HashMap::new(),
    };
    match toml::from_str::<toml::Value>(raw) {
        Ok(parsed) => {
            let mut map = HashMap::new();
            flatten(&parsed, "", &mut map);
            map
        }
        Err(e) => {
            log_info!("i18n: failed to parse locale '{}': {}", locale, e);
            HashMap::new()
        }
    }
}

// en is the fallback locale; constant, parsed only once.
static EN_MESSAGES: LazyLock<HashMap<String, String>> = LazyLock::new(|| load_messages("en"));

struct I18nState {
    locale: String,                    // resolved locale, e.g. "zh-Hans"
    messages: HashMap<String, String>, // flat key->string map for the current locale (identical to EN_MESSAGES when locale == "en")
}

// Init reads only the system locale, NOT CONFIG (see cycle note at file top).
static I18N: LazyLock<RwLock<I18nState>> = LazyLock::new(|| {
    let locale = resolve_locale(None);
    RwLock::new(I18nState {
        messages: load_messages(&locale),
        locale,
    })
});

/// Simple lookup: current locale -> en fallback -> the key itself.
pub fn t(key: &str) -> String {
    let g = I18N.read().unwrap();
    let value = g
        .messages
        .get(key)
        .cloned()
        .or_else(|| EN_MESSAGES.get(key).cloned())
        .unwrap_or_else(|| key.to_string());
    #[cfg(any(debug_assertions, feature = "dev-long-text"))]
    let is_long_test_locale = g.locale == TEST_LONG_LOCALE;
    drop(g);
    #[cfg(any(debug_assertions, feature = "dev-long-text"))]
    {
        if is_long_test_locale {
            return long_test_localize_text(&value);
        }
        if pseudo_locale_enabled() {
            return pseudo_localize_text(&value);
        }
    }
    value
}

/// Return the resolved locale currently used for localized UI strings.
pub fn current_locale() -> String {
    I18N.read().unwrap().locale.clone()
}

/// Enable long-text layout QA without adding a fake production locale. Set
/// `--pseudo-locale` (optionally `=1/true/yes/on`) before launching the app; placeholders such
/// as `{count}` remain byte-for-byte intact so `tf` can still interpolate runtime values.
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
fn pseudo_locale_enabled() -> bool {
    crate::dev_flags::enabled(PSEUDO_LOCALE_FLAG)
}

/// Repeat English UI strings for the debug-only language-menu layout fixture.
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
fn long_test_localize_text(s: &str) -> String {
    format!("{s} {s} {s}")
}

/// Keep the punctuation-based QA helper out of production builds.
#[cfg(any(debug_assertions, feature = "dev-long-text"))]
fn pseudo_localize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2 + 4);
    out.push('[');
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            out.push(ch);
            for token in chars.by_ref() {
                out.push(token);
                if token == '}' {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
        if ch.is_ascii_alphabetic() {
            out.push('·');
        }
    }
    out.push(']');
    out
}

/// Lookup with interpolation: replace {name} placeholders in the template with args.
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Count-aware lookup: count == 1 selects "{key}_one", otherwise "{key}_other", and
/// interpolates {count}. Languages without a plural distinction (e.g. Chinese) still declare
/// both keys with the same value so the locale key-parity test keeps covering them.
pub fn t_count(key: &str, count: usize) -> String {
    let suffix = if count == 1 { "_one" } else { "_other" };
    tf(&format!("{key}{suffix}"), &[("count", &count.to_string())])
}

/// Apply the locale from config (called by config.rs after CONFIG init and after reload).
/// locale_cfg "auto" means follow the system language; other values must be in the
/// supported list, otherwise fall back to auto.
pub fn apply_config_locale(locale_cfg: &str) {
    let resolved = resolve_locale(Some(locale_cfg));
    let mut g = I18N.write().unwrap();
    if g.locale == resolved {
        return; // unchanged, skip recompute
    }
    g.locale = resolved.clone();
    g.messages = load_messages(&resolved);
}

/// Resolve the final locale. Priority: locale_cfg (non-auto & supported) > first system
/// preferred language that maps to a supported locale > DEFAULT_LOCALE.
fn resolve_locale(locale_cfg: Option<&str>) -> String {
    resolve_locale_from(locale_cfg, &system_locales())
}

/// Resolve the final locale from a config value and an injected system-language list
/// (pure; tests feed their own lists instead of the real NSLocale).
fn resolve_locale_from(locale_cfg: Option<&str>, system: &[String]) -> String {
    if let Some(cfg) = locale_cfg {
        if cfg != "auto" && is_supported(cfg) {
            return cfg.to_string();
        }
    }
    // Iterate the system preferred-language list in order; return the first that maps to a
    // supported locale. Iterating (instead of taking only the first) ensures a supported
    // language lower in the user's preference is chosen over the default fallback: e.g. for
    // preference order [ja, zh-Hans, en] we pick zh-Hans, not en.
    for tag in system {
        if let Some(loc) = map_tag_to_supported(tag) {
            return loc.to_string();
        }
    }
    DEFAULT_LOCALE.to_string()
}

/// Map a single system language tag to a supported locale, or None if unsupported.
/// Chinese splits into Simplified/Traditional: Hant script or region TW/HK/MO -> Traditional;
/// everything else (Hans, CN, SG, bare zh) -> Simplified.
fn map_tag_to_supported(tag: &str) -> Option<&'static str> {
    let lower = tag.to_lowercase();
    if lower.starts_with("zh") {
        if lower.contains("hant")
            || lower.contains("tw")
            || lower.contains("hk")
            || lower.contains("mo")
        {
            Some("zh-Hant")
        } else {
            Some("zh-Hans")
        }
    } else if lower.starts_with("en") {
        Some("en")
    } else {
        None
    }
}

/// Read the system's preferred-language list (NSLocale preferredLanguages, ordered, first is
/// most preferred). NSLocale is a Foundation class usable without NSApplication running, so
/// triggering I18N during CONFIG init is safe. preferredLanguages / objectAtIndex: follow the
/// Get rule (+0 autoreleased), so no release is needed.
fn system_locales() -> Vec<String> {
    unsafe {
        let arr: *mut AnyObject = msg_send![class!(NSLocale), preferredLanguages];
        if arr.is_null() {
            return Vec::new();
        }
        let count: usize = msg_send![arr, count];
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let s: *mut AnyObject = msg_send![arr, objectAtIndex: i];
            out.push(nsstring_to_rust(s));
        }
        out
    }
}

unsafe fn nsstring_to_rust(ns: *mut AnyObject) -> String {
    if ns.is_null() {
        return String::new();
    }
    let utf8: *const c_char = msg_send![ns, UTF8String];
    if utf8.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr(utf8)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|s| s.to_string()).collect()
    }

    // Extract the names of all {name} placeholders in a string.
    fn placeholders(s: &str) -> std::collections::HashSet<&str> {
        let mut set = std::collections::HashSet::new();
        for part in s.split('{').skip(1) {
            if let Some(end) = part.find('}') {
                set.insert(&part[..end]);
            }
        }
        set
    }

    #[test]
    fn explicit_config_wins_over_system() {
        // An explicit supported config locale beats the system list.
        let sys = list(&["ja", "zh-Hans", "en"]);
        assert_eq!(resolve_locale_from(Some("en"), &sys), "en");
        assert_eq!(resolve_locale_from(Some("zh-Hant"), &sys), "zh-Hant");
    }

    #[test]
    fn auto_or_unsupported_falls_back_to_system() {
        // "auto" and unsupported config values fall back to the system list.
        let sys = list(&["ja", "zh-Hans", "en"]);
        assert_eq!(resolve_locale_from(Some("auto"), &sys), "zh-Hans");
        assert_eq!(resolve_locale_from(Some("fr"), &sys), "zh-Hans");
        assert_eq!(resolve_locale_from(None, &sys), "zh-Hans");
    }

    #[test]
    fn lower_preference_supported_locale_wins_over_default() {
        // A supported language lower in the preference list is still chosen over en.
        assert_eq!(
            resolve_locale_from(Some("auto"), &list(&["ja", "zh-Hant", "en"])),
            "zh-Hant"
        );
        assert_eq!(resolve_locale_from(None, &list(&["fr", "de", "en"])), "en");
    }

    #[test]
    fn empty_system_list_falls_back_to_default() {
        assert_eq!(resolve_locale_from(None, &Vec::new()), "en");
    }

    #[test]
    fn map_tag_covers_chinese_variants() {
        // Script/region both decide Simplified vs Traditional.
        assert_eq!(map_tag_to_supported("zh"), Some("zh-Hans"));
        assert_eq!(map_tag_to_supported("zh-CN"), Some("zh-Hans"));
        assert_eq!(map_tag_to_supported("zh-SG"), Some("zh-Hans"));
        assert_eq!(map_tag_to_supported("zh-Hans"), Some("zh-Hans"));
        assert_eq!(map_tag_to_supported("zh-Hant"), Some("zh-Hant"));
        assert_eq!(map_tag_to_supported("zh-TW"), Some("zh-Hant"));
        assert_eq!(map_tag_to_supported("zh-HK"), Some("zh-Hant"));
        assert_eq!(map_tag_to_supported("zh-MO"), Some("zh-Hant"));
        assert_eq!(map_tag_to_supported("en"), Some("en"));
        assert_eq!(map_tag_to_supported("en-US"), Some("en"));
        assert_eq!(map_tag_to_supported("ja"), None);
        assert_eq!(map_tag_to_supported("de-DE"), None);
        assert_eq!(map_tag_to_supported(""), None);
    }

    #[test]
    fn flatten_nests_tables_with_dot_keys() {
        let mut out = HashMap::new();
        flatten(
            &toml::from_str(
                r#"
[menu]
settings = "Settings"
[menu.sub]
nested = "x"
number = 42
"#,
            )
            .unwrap(),
            "",
            &mut out,
        );
        assert_eq!(
            out.get("menu.settings").map(String::as_str),
            Some("Settings")
        );
        assert_eq!(out.get("menu.sub.nested").map(String::as_str), Some("x"));
        // Non-string leaves are ignored.
        assert!(!out.contains_key("menu.sub.number"));
    }

    #[test]
    fn tf_replaces_all_placeholders() {
        // All {name} placeholders are replaced.
        let s = tf("settings.version_label", &[("version", "0.1.4")]);
        assert!(!s.contains('{'));
        assert!(s.contains("0.1.4"));
        // A missing argument leaves the placeholder untouched.
        let s2 = tf("settings.version_label", &[]);
        assert!(s2.contains('{'));
    }

    #[test]
    fn count_keys_exist_with_distinct_singular_and_plural_forms() {
        // Singular and plural must be distinct English strings, otherwise a "1 items"
        // regression would slip through unnoticed.
        for key in [
            "clipboard.footer_count",
            "clipboard.detail_lines",
            "clipboard.detail_chars",
        ] {
            let one = EN_MESSAGES
                .get(&format!("{key}_one"))
                .unwrap_or_else(|| panic!("missing {key}_one"));
            let other = EN_MESSAGES
                .get(&format!("{key}_other"))
                .unwrap_or_else(|| panic!("missing {key}_other"));
            assert_ne!(one, other, "{key} needs distinct singular/plural forms");
            assert_eq!(
                placeholders(one),
                placeholders(other),
                "{key} placeholder drift"
            );
        }
    }

    #[test]
    fn t_count_interpolates_the_count_placeholder() {
        // Locale-independent: both forms must resolve {count} (true even where they are equal).
        for count in [0usize, 1, 2] {
            let s = t_count("clipboard.detail_lines", count);
            assert!(!s.contains('{'), "unresolved placeholder for {count}: {s}");
            assert!(
                s.contains(&count.to_string()),
                "missing count for {count}: {s}"
            );
        }
    }

    #[test]
    fn all_locales_share_identical_key_sets() {
        // All locale files must expose the exact same key set (no missing/extra keys).
        let keys = |raw: &str| {
            let parsed: toml::Value = toml::from_str(raw).unwrap();
            let mut map = HashMap::new();
            flatten(&parsed, "", &mut map);
            map
        };
        let en = keys(EN_TOML);
        let zh = keys(ZH_TOML);
        let zh_hant = keys(ZH_HANT_TOML);
        assert!(!en.is_empty(), "en locale must not be empty");
        let en_set: std::collections::HashSet<&String> = en.keys().collect();
        let zh_set: std::collections::HashSet<&String> = zh.keys().collect();
        let hant_set: std::collections::HashSet<&String> = zh_hant.keys().collect();
        assert_eq!(en_set, zh_set, "zh-Hans keys differ from en");
        assert_eq!(zh_set, hant_set, "zh-Hant keys differ from zh-Hans");
        // Every key has a non-empty value in zh (no empty translations).
        for (k, v) in &zh {
            assert!(!v.is_empty(), "empty translation for key {}", k);
        }
    }

    #[test]
    fn placeholder_parity_across_locales() {
        // Placeholders ({name}) must match across locales for every key: a translation
        // missing a placeholder would silently bake user data (e.g. a version number)
        // into the text -- the key-set test cannot catch that.
        let parse = |raw: &str| {
            let parsed: toml::Value = toml::from_str(raw).unwrap();
            let mut map = HashMap::new();
            flatten(&parsed, "", &mut map);
            map
        };
        let en = parse(EN_TOML);
        let zh = parse(ZH_TOML);
        let hant = parse(ZH_HANT_TOML);
        for (k, v) in &en {
            let expected = placeholders(v);
            if expected.is_empty() {
                continue;
            }
            let zh_ph = placeholders(&zh[k]);
            let hant_ph = placeholders(&hant[k]);
            assert_eq!(
                zh_ph, expected,
                "zh-Hans placeholder mismatch for key {} (en: {:?}, zh: {:?})",
                k, v, zh[k]
            );
            assert_eq!(
                hant_ph, expected,
                "zh-Hant placeholder mismatch for key {} (en: {:?}, hant: {:?})",
                k, v, hant[k]
            );
        }
    }

    #[cfg(any(debug_assertions, feature = "dev-long-text"))]
    #[test]
    fn pseudo_localization_expands_strings_without_corrupting_placeholders() {
        // Exercise every English UI string with expansion and placeholder preservation so future
        // layout changes have a deterministic long-text fixture without shipping a fake locale.
        let mut expanded = 0usize;
        for (key, value) in EN_MESSAGES.iter() {
            let pseudo = pseudo_localize_text(value);
            assert!(pseudo.len() >= value.len(), "pseudo text shrank for {key}");
            assert_eq!(
                placeholders(&pseudo),
                placeholders(value),
                "placeholder drift for {key}"
            );
            if value.chars().count() >= 8 {
                assert!(
                    pseudo.chars().count() > value.chars().count(),
                    "no expansion for {key}"
                );
                expanded += 1;
            }
        }
        assert!(
            expanded > 20,
            "fixture should cover a broad set of UI strings"
        );
    }

    #[cfg(any(debug_assertions, feature = "dev-long-text"))]
    #[test]
    fn debug_long_locale_repeats_english_text_three_times() {
        assert_eq!(
            resolve_locale_from(Some(TEST_LONG_LOCALE), &list(&["zh-Hans"])),
            TEST_LONG_LOCALE
        );
        assert_eq!(
            long_test_localize_text("Show icons and thumbnails"),
            "Show icons and thumbnails Show icons and thumbnails Show icons and thumbnails"
        );
    }
}
