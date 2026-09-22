//! 开发/验证开关的唯一入口:只认 argv,不读环境变量。
//!
//! 为什么不走环境变量:开发版由 `launchctl submit` 交给 launchd 启动,而 launchd 任务
//! **不继承调用者的 shell 环境**。历史上我们用白名单把 `OH_MY_TAB_*` 转发进任务,但那条路
//! 有两个问题:一是白名单一旦写漏就把整份环境(云凭证、代理)灌进任务和日志,2026-09-22 出过
//! 一次事故;二是开关散落在环境变量里,验证时看不见、说不清。现在统一走 argv:
//! `scripts/dev-restart.sh --flag[=value]`,脚本对不认识的 `--*` 参数原样透传给应用。
//!
//! 认识两种写法:`--name`(裸开关,值按空串处理)与 `--name=value`。
//!
//! The single entry point for development/verification switches: argv only, never the
//! environment. The development build is started by launchd via `launchctl submit`, and a launchd
//! job does not inherit the caller's shell environment. An allowlist used to forward `OH_MY_TAB_*`
//! into the job, which had two problems: a filter mistake dumped the whole environment (cloud
//! credentials, proxies) into the job and the logs once, and switches hidden in the environment are
//! invisible when you verify a change. They ride argv now:
//! `scripts/dev-restart.sh --flag[=value]`, with the script forwarding any `--*` argument it does
//! not own straight to the app. Both `--name` (bare switch, empty value) and `--name=value` work.

use std::sync::OnceLock;

/// 应用参数(argv[0] 之后),只读一次。解析本身是纯函数,便于单测。
/// The app's arguments (after argv[0]), read once. Parsing is a pure function so it stays unit
/// testable.
fn args() -> &'static [String] {
    static ARGS: OnceLock<Vec<String>> = OnceLock::new();
    ARGS.get_or_init(|| std::env::args().skip(1).collect())
}

/// 在给定参数里取 `--name=value`:裸 `--name` 返回 `Some("")`,未出现返回 `None`。
/// Reads `--name=value` from the given arguments: a bare `--name` yields `Some("")`, absence
/// yields `None`.
fn value_in(args: &[String], name: &str) -> Option<String> {
    let flag = format!("--{name}");
    for arg in args {
        if arg == &flag {
            return Some(String::new());
        }
        // 只认 `--name=` 这种精确分界,`--namefoo` 不会被误判成 `--name`。
        // Only the exact `--name=` boundary counts, so `--namefoo` is not mistaken for `--name`.
        if let Some(rest) = arg.strip_prefix(&flag) {
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// `--name` / `--name=value` 是否出现(值不重要时用这个)。
/// Whether `--name` / `--name=value` appears (use when the value does not matter).
pub(crate) fn present(name: &str) -> bool {
    value(name).is_some()
}

/// 取 `--name=value` 的值(裸 `--name` 为 `Some("")`)。
/// The value of `--name=value` (a bare `--name` gives `Some("")`).
pub(crate) fn value(name: &str) -> Option<String> {
    value_in(args(), name)
}

/// 布尔开关:裸 `--name`,或 `--name=1/true/yes/on`。
/// A boolean switch: bare `--name`, or `--name=1/true/yes/on`.
fn enabled_in(args: &[String], name: &str) -> bool {
    match value_in(args, name) {
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "1" | "true" | "yes" | "on"
        ),
        None => false,
    }
}

/// 读取真实启动参数里的布尔开关。
/// Reads a boolean switch from the real launch arguments.
pub(crate) fn enabled(name: &str) -> bool {
    enabled_in(args(), name)
}

/// 是否有参数以给定前缀开头(用于 `--smoke*` 这类一族开关)。
/// Whether any argument starts with the given prefix (for families such as `--smoke*`).
pub(crate) fn any_prefix(prefix: &str) -> bool {
    args().iter().any(|arg| arg.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn bare_flag_yields_an_empty_value() {
        let args = argv(&["--force-onboarding"]);
        assert_eq!(value_in(&args, "force-onboarding"), Some(String::new()));
        assert!(value_in(&args, "other").is_none());
    }

    #[test]
    fn valued_flag_keeps_everything_after_the_first_equals() {
        let args = argv(&["--fake-permissions=ax:0,sr:0", "--open-settings="]);
        assert_eq!(
            value_in(&args, "fake-permissions"),
            Some("ax:0,sr:0".to_string())
        );
        // `--open-settings=` 是显式空值,与裸 `--open-settings` 等价。
        // `--open-settings=` is an explicit empty value, equivalent to a bare flag.
        assert_eq!(value_in(&args, "open-settings"), Some(String::new()));
    }

    #[test]
    fn similar_names_do_not_collide() {
        let args = argv(&["--open-settings-extra=1", "--open-settings"]);
        // 前缀更长的那一个不会被当成 `open-settings`。
        // The longer-prefixed argument is not treated as `open-settings`.
        assert_eq!(value_in(&args, "open-settings"), Some(String::new()));
        assert_eq!(value_in(&args, "open"), None);
    }

    #[test]
    fn enabled_accepts_only_explicit_truthy_values() {
        assert!(enabled_in(&argv(&["--pseudo-locale"]), "pseudo-locale"));
        for truthy in ["1", "true", "YES", "on"] {
            let args = argv(&[&format!("--pseudo-locale={truthy}")]);
            assert!(enabled_in(&args, "pseudo-locale"), "{truthy} should enable");
        }
        for falsy in ["0", "no", "off", "2"] {
            let args = argv(&[&format!("--pseudo-locale={falsy}")]);
            assert!(
                !enabled_in(&args, "pseudo-locale"),
                "{falsy} must not enable"
            );
        }
    }

    #[test]
    fn arguments_without_the_prefix_are_ignored() {
        let args = argv(&["--smoke-settings-layout", "positional"]);
        assert!(value_in(&args, "smoke-settings-layout").is_some());
        assert!(value_in(&args, "positional").is_none());
    }
}
