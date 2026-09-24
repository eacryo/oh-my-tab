//! The single entry point for development/verification switches: argv only, never the
//! environment. The development build is started by launchd via `launchctl submit`, and a launchd
//! job does not inherit the caller's shell environment. An allowlist used to forward `OH_MY_TAB_*`
//! into the job, which had two problems: a filter mistake dumped the whole environment (cloud
//! credentials, proxies) into the job and the logs once, and switches hidden in the environment are
//! invisible when you verify a change. They ride argv now:
//! `scripts/dev-restart.sh --flag[=value]`, with the script forwarding any `--*` argument it does
//! not own straight to the app. Both `--name` (bare switch, empty value) and `--name=value` work.

use std::sync::OnceLock;

/// The app's arguments (after argv[0]), read once. Parsing is a pure function so it stays unit
/// testable.
fn args() -> &'static [String] {
    static ARGS: OnceLock<Vec<String>> = OnceLock::new();
    ARGS.get_or_init(|| std::env::args().skip(1).collect())
}

/// Reads `--name=value` from the given arguments: a bare `--name` yields `Some("")`, absence
/// yields `None`.
fn value_in(args: &[String], name: &str) -> Option<String> {
    let flag = format!("--{name}");
    for arg in args {
        if arg == &flag {
            return Some(String::new());
        }
        // Only the exact `--name=` boundary counts, so `--namefoo` is not mistaken for `--name`.
        if let Some(rest) = arg.strip_prefix(&flag) {
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Whether `--name` / `--name=value` appears (use when the value does not matter).
pub(crate) fn present(name: &str) -> bool {
    value(name).is_some()
}

/// The value of `--name=value` (a bare `--name` gives `Some("")`).
pub(crate) fn value(name: &str) -> Option<String> {
    value_in(args(), name)
}

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

/// Reads a boolean switch from the real launch arguments.
pub(crate) fn enabled(name: &str) -> bool {
    enabled_in(args(), name)
}

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
        // `--open-settings=` is an explicit empty value, equivalent to a bare flag.
        assert_eq!(value_in(&args, "open-settings"), Some(String::new()));
    }

    #[test]
    fn similar_names_do_not_collide() {
        let args = argv(&["--open-settings-extra=1", "--open-settings"]);
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
