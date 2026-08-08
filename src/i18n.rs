// i18n 模块:手搓的 TOML 国际化体系,与 config.rs 同构。
// 零新依赖(toml/serde 已有),翻译文件编译期内嵌,locale 由 config 驱动、可热重载。
//
// i18n module: a handcrafted TOML-based localization system, isomorphic to config.rs.
// Zero new deps (toml/serde already present); locale files are embedded at compile time;
// the active locale is config-driven and hot-reloadable.
//
// 循环依赖说明:本模块绝不读取 CONFIG,只读系统语言(NSLocale)。这样 CONFIG 的
// LazyLock 初始化期间若调用 validate() -> t() -> I18N 初始化,不会形成死锁。
// config.rs 在 CONFIG 初始化与 reload 后单向调用 apply_config_locale() 应用配置覆盖。
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

// 翻译文件编译期内嵌,避免运行时缺文件 / 读取失败。
// Locale files embedded at compile time to avoid runtime file-missing / read failures.
const EN_TOML: &str = include_str!("../locales/en.toml");
const ZH_TOML: &str = include_str!("../locales/zh-Hans.toml");
const ZH_HANT_TOML: &str = include_str!("../locales/zh-Hant.toml");
const DEFAULT_LOCALE: &str = "en";

// 已支持的 locale -> 内嵌 TOML 文本。新增语言只需加文件 + 在此登记。
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
    locale_raw(locale).is_some()
}

// 把嵌套 TOML 表扁平化成 "section.key" -> value 的映射,只收字符串叶节点。
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
                _ => {} // 忽略非字符串叶节点 / ignore non-string leaves
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

// en 是兜底 locale,常量,只解析一次。
// en is the fallback locale; constant, parsed only once.
static EN_MESSAGES: LazyLock<HashMap<String, String>> = LazyLock::new(|| load_messages("en"));

struct I18nState {
    locale: String,                    // 已解析的实际 locale,如 "zh-Hans"
    messages: HashMap<String, String>, // 当前 locale 的扁平 key->string(locale=="en" 时与 EN_MESSAGES 相同)
}

// 初始化只读系统语言,不读 CONFIG(见文件头循环依赖说明)。
// Init reads only the system locale, NOT CONFIG (see cycle note at file top).
static I18N: LazyLock<RwLock<I18nState>> = LazyLock::new(|| {
    let locale = resolve_locale(None);
    RwLock::new(I18nState {
        messages: load_messages(&locale),
        locale,
    })
});

/// 简单查表:当前 locale -> en 兜底 -> key 本身。
/// Simple lookup: current locale -> en fallback -> the key itself.
pub fn t(key: &str) -> String {
    let g = I18N.read().unwrap();
    if let Some(v) = g.messages.get(key) {
        return v.clone();
    }
    drop(g);
    if let Some(v) = EN_MESSAGES.get(key) {
        return v.clone();
    }
    key.to_string()
}

/// 带插值的查表:把模板里的 {name} 替换为 args 提供的值。
/// Lookup with interpolation: replace {name} placeholders in the template with args.
pub fn tf(key: &str, args: &[(&str, &str)]) -> String {
    let mut s = t(key);
    for (k, v) in args {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// 应用 config 里的 locale 配置(由 config.rs 在 CONFIG 初始化与 reload 后调用)。
/// locale_cfg 为 "auto" 表示跟随系统语言;其它值须在支持列表内,否则回退 auto。
///
/// Apply the locale from config (called by config.rs after CONFIG init and after reload).
/// locale_cfg "auto" means follow the system language; other values must be in the
/// supported list, otherwise fall back to auto.
pub fn apply_config_locale(locale_cfg: &str) {
    let resolved = resolve_locale(Some(locale_cfg));
    let mut g = I18N.write().unwrap();
    if g.locale == resolved {
        return; // 未变,避免无谓重算 / unchanged, skip recompute
    }
    g.locale = resolved.clone();
    g.messages = load_messages(&resolved);
}

/// 解析最终 locale。优先级:locale_cfg(非 auto 且在支持列表) > 系统偏好语言列表中首个
/// 能映射到已支持 locale 的项 > DEFAULT_LOCALE。
/// Resolve the final locale. Priority: locale_cfg (non-auto & supported) > first system
/// preferred language that maps to a supported locale > DEFAULT_LOCALE.
fn resolve_locale(locale_cfg: Option<&str>) -> String {
    resolve_locale_from(locale_cfg, &system_locales())
}

/// 从配置值与注入的系统语言列表解析最终 locale(纯函数,测试可直接喂列表)。
/// Resolve the final locale from a config value and an injected system-language list
/// (pure; tests feed their own lists instead of the real NSLocale).
fn resolve_locale_from(locale_cfg: Option<&str>, system: &[String]) -> String {
    if let Some(cfg) = locale_cfg {
        if cfg != "auto" && is_supported(cfg) {
            return cfg.to_string();
        }
    }
    // 按顺序遍历系统偏好语言,返回第一个能映射到已支持 locale 的项。
    // 遍历(而非只取首项)确保用户次优偏好里的已支持语言被选中,而不是直接回退默认:
    // 例如偏好顺序为 [ja, zh-Hans, en] 时,选中 zh-Hans 而非 en。
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

/// 把单个系统语言标签映射到已支持的 locale,未匹配返回 None。
/// 中文区分简体/繁体:含 Hant 或区域为 TW/HK/MO 视为繁体;其余(含 Hans、CN、SG、纯 zh)为简体。
///
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

/// 取系统偏好语言列表(NSLocale preferredLanguages,有序,首项最优先)。NSLocale 是
/// Foundation 类,无需 NSApplication 运行即可用,因此 I18N 在 CONFIG 初始化期间被触发也安全。
/// preferredLanguages / objectAtIndex: 遵循 Get 规则(+0 autoreleased),无需 release。
///
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

    // 提取字符串里所有 {name} 占位符的名字。
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
        // 显式配置(受支持)优先于系统语言。
        // An explicit supported config locale beats the system list.
        let sys = list(&["ja", "zh-Hans", "en"]);
        assert_eq!(resolve_locale_from(Some("en"), &sys), "en");
        assert_eq!(resolve_locale_from(Some("zh-Hant"), &sys), "zh-Hant");
    }

    #[test]
    fn auto_or_unsupported_falls_back_to_system() {
        // auto 与不支持的配置值都走系统语言。
        // "auto" and unsupported config values fall back to the system list.
        let sys = list(&["ja", "zh-Hans", "en"]);
        assert_eq!(resolve_locale_from(Some("auto"), &sys), "zh-Hans");
        assert_eq!(resolve_locale_from(Some("fr"), &sys), "zh-Hans");
        assert_eq!(resolve_locale_from(None, &sys), "zh-Hans");
    }

    #[test]
    fn lower_preference_supported_locale_wins_over_default() {
        // 支持的语言排在偏好列表靠后时仍应被选中,而不是直接回退 en。
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
        // 简/繁拆分:区域与脚本都影响结果。
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
        // 非字符串叶节点被忽略。
        // Non-string leaves are ignored.
        assert!(!out.contains_key("menu.sub.number"));
    }

    #[test]
    fn tf_replaces_all_placeholders() {
        // 模板插值:所有 {name} 都被替换。
        // All {name} placeholders are replaced.
        let s = tf("settings.version_label", &[("version", "0.1.4")]);
        assert!(!s.contains('{'));
        assert!(s.contains("0.1.4"));
        // 缺失参数保持原样(占位符不消失)。
        // A missing argument leaves the placeholder untouched.
        let s2 = tf("settings.version_label", &[]);
        assert!(s2.contains('{'));
    }

    #[test]
    fn all_locales_share_identical_key_sets() {
        // 三份 locale 文件的 key 集合必须完全一致,防止漏翻译/多翻译。
        // All locale files must expose the exact same key set (no missing/extra keys).
        let mut keys = |raw: &str| {
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
        // 每个 key 在 zh 里都有非空值(不做空翻译)。
        // Every key has a non-empty value in zh (no empty translations).
        for (k, v) in &zh {
            assert!(!v.is_empty(), "empty translation for key {}", k);
        }
    }

    #[test]
    fn placeholder_parity_across_locales() {
        // 同一 key 的 {name} 占位符必须跨语言一致:某语言漏写占位符会静默把
        // 用户数据(如版本号)硬编码成翻译的一部分,key 集合测试抓不到这种错。
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
}
