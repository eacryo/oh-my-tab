//! The terminal applications the "Open in Terminal" items can use.
//!
//! The list is an identity table, not a discovery mechanism: the config stores bundle ids from it,
//! the settings page shows the installed ones, and the launcher has one adapter per entry.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

use crate::config::FINDER_MENU_TERMINALS;
use crate::ffi::make_nsstring;

/// One known terminal application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalSpec {
    /// The bundle identifier the config stores.
    pub(crate) bundle_id: &'static str,
    /// The product name shown in menus and settings rows (data, not UI chrome).
    pub(crate) name: &'static str,
}

/// How a terminal is asked to open a directory. The two shapes differ because a running terminal
/// usually handles a folder URL itself, while some terminals only accept a fresh instance with an
/// argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchStyle {
    /// Open the directory URL with the application; the running instance handles it.
    FolderUrl,
    /// Launch a new instance with these arguments, substituting the directory for {dir}.
    Arguments(&'static [&'static str]),
}

const TERMINALS: [TerminalSpec; 7] = [
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[0],
        name: "Terminal",
    },
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[1],
        name: "iTerm2",
    },
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[2],
        name: "Ghostty",
    },
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[3],
        name: "Warp",
    },
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[4],
        name: "WezTerm",
    },
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[5],
        name: "kitty",
    },
    TerminalSpec {
        bundle_id: FINDER_MENU_TERMINALS[6],
        name: "Alacritty",
    },
];

/// How each terminal is launched. Entries marked as unverified have not been exercised against the
/// running application yet; the settings page still offers them once installed, and the launcher
/// falls back to opening the folder URL if the argument form fails.
pub(crate) fn launch_style(bundle_id: &str) -> Option<LaunchStyle> {
    Some(match bundle_id {
        "com.apple.Terminal" | "com.googlecode.iterm2" | "dev.warp.Warp-Stable" => {
            LaunchStyle::FolderUrl
        }
        "com.mitchellh.ghostty" => LaunchStyle::Arguments(&["--working-directory={dir}"]),
        "com.github.wez.wezterm" => LaunchStyle::Arguments(&["start", "--cwd", "{dir}"]),
        "net.kovidgoyal.kitty" => LaunchStyle::Arguments(&["--directory", "{dir}"]),
        "org.alacritty" => LaunchStyle::Arguments(&["--working-directory", "{dir}"]),
        _ => return None,
    })
}

pub(crate) fn catalog() -> &'static [TerminalSpec] {
    &TERMINALS
}

/// The spec for a bundle id, if it is one of the known terminals.
pub(crate) fn spec(bundle_id: &str) -> Option<&'static TerminalSpec> {
    TERMINALS.iter().find(|spec| spec.bundle_id == bundle_id)
}

/// Whether the application for this bundle id is installed. The lookup is a LaunchServices query,
/// so it answers for installed-but-not-running applications too.
pub(crate) fn is_installed(bundle_id: &str) -> bool {
    application_path(bundle_id).is_some()
}
/// Convert a launch style into the arguments of `open(1)`. Pure, so the quoting-sensitive part is
/// testable.
pub(crate) fn open_arguments(style: LaunchStyle, app_path: &str, directory: &str) -> Vec<String> {
    match style {
        LaunchStyle::FolderUrl => vec![
            "-a".to_string(),
            app_path.to_string(),
            directory.to_string(),
        ],
        LaunchStyle::Arguments(template) => {
            let mut arguments = vec![
                "-na".to_string(),
                app_path.to_string(),
                "--args".to_string(),
            ];
            arguments.extend(
                template
                    .iter()
                    .map(|argument| argument.replace("{dir}", directory)),
            );
            arguments
        }
    }
}

/// Open `directory` in the terminal named by `bundle_id`.
pub(crate) fn launch(bundle_id: &str, directory: &std::path::Path) -> std::io::Result<()> {
    let Some(spec) = spec(bundle_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "unknown terminal",
        ));
    };
    if !directory.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "target directory does not exist",
        ));
    }
    let Some(app_path) = application_path(spec.bundle_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "terminal is not installed",
        ));
    };
    let Some(style) = launch_style(spec.bundle_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no launch adapter",
        ));
    };
    let arguments = open_arguments(style, &app_path, &directory.to_string_lossy());
    let status = std::process::Command::new("/usr/bin/open")
        .args(&arguments)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("open exited with {status}")))
    }
}

/// The absolute path of an installed application, when LaunchServices knows one.
fn application_path(bundle_id: &str) -> Option<String> {
    unsafe {
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let identifier = make_nsstring(bundle_id);
        let url: *mut AnyObject =
            msg_send![workspace, URLForApplicationWithBundleIdentifier: identifier];
        crate::ffi::release_obj(identifier);
        if url.is_null() {
            return None;
        }
        let path: *mut AnyObject = msg_send![url, path];
        if path.is_null() {
            return None;
        }
        Some(crate::ffi::nsstring_to_rust(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_url_style_passes_the_directory_to_open() {
        assert_eq!(
            open_arguments(LaunchStyle::FolderUrl, "/Applications/Ghostty.app", "/tmp"),
            vec!["-a", "/Applications/Ghostty.app", "/tmp"]
        );
    }

    #[test]
    fn argument_style_starts_a_new_instance_with_the_directory_substituted() {
        let arguments = open_arguments(
            LaunchStyle::Arguments(&["--working-directory={dir}"]),
            "/Applications/Ghostty.app",
            "/tmp/with space",
        );
        assert_eq!(
            arguments,
            vec![
                "-na",
                "/Applications/Ghostty.app",
                "--args",
                "--working-directory=/tmp/with space"
            ]
        );
    }

    #[test]
    fn every_known_terminal_has_a_launch_style() {
        for spec in catalog() {
            assert!(
                launch_style(spec.bundle_id).is_some(),
                "{} has no adapter",
                spec.bundle_id
            );
        }
    }

    #[test]
    fn unknown_bundle_ids_have_no_adapter() {
        assert!(launch_style("com.example.not-a-terminal").is_none());
    }
}
