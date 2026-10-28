//! Workspace release-version consistency.
//!
//! Every packaging input (the app's `Info.plist`, the Finder extension's nested `Info.plist`, the
//! Sparkle appcast and the Homebrew cask) derives its version from the ROOT `Cargo.toml`. The two other
//! workspace members carry their own `version` field, which nothing rewrites -- so a release that bumps
//! only the root manifest ships an `.app` that says (for example) 0.2.6 while the extension crate it
//! embeds still says 0.2.5, and no build, bundle or publish step notices.
//!
//! The check lives here, as a tier-A test, rather than in a release script: a release-only check would
//! never run while developing, and the breakage is silent until someone reads the crate metadata.
//!
//! It refuses to report agreement from an empty parse -- a version comparison that found no versions
//! would otherwise pass silently, which is exactly the failure this module exists to catch.

#[cfg(test)]
use std::path::Path;

/// The workspace root, as compiled in by the root package.
#[cfg(test)]
const WORKSPACE_ROOT: &str = env!("CARGO_MANIFEST_DIR");

/// The version this build of the app was compiled at.
#[cfg(test)]
pub(crate) const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The workspace members whose manifest carries its own version. The root package is `.`, whose
/// version IS the expected value, so it is not listed.
#[cfg(test)]
pub(crate) const WORKSPACE_MEMBERS: &[&str] =
    &["finder-ext/Cargo.toml", "crates/finder-protocol/Cargo.toml"];

/// A diagnostic for a manifest whose version cannot be read, or that contains no `[package]` table.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManifestError {
    Parse { path: String, reason: String },
    Read { path: String, reason: String },
    NoPackageTable { path: String },
}

#[cfg(test)]
impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Parse { path, reason } => {
                write!(f, "{path}: cannot parse the manifest ({reason})")
            }
            ManifestError::Read { path, reason } => {
                write!(f, "{path}: cannot read the manifest ({reason})")
            }
            ManifestError::NoPackageTable { path } => {
                write!(
                    f,
                    "{path}: no [package] table (a virtual manifest has no version)"
                )
            }
        }
    }
}

/// The version of a manifest's own `[package]` table.
///
/// Parsed as TOML rather than scanned line by line: a hand-rolled scan reads `version = "..."` inside
/// a multi-line string, a comment body or an array element as if it were the package's version, and
/// would then report agreement for a tree that disagrees.
///
/// An inherited version (`version.workspace = true`) and a missing, non-string or empty version are
/// all errors: this check compares literal versions, and a form it cannot read must be reported
/// rather than skipped.
#[cfg(test)]
pub(crate) fn package_version(manifest: &str) -> Result<String, String> {
    let parsed = manifest
        .parse::<toml::Value>()
        .map_err(|err| format!("invalid TOML: {err}"))?;
    let package = parsed
        .get("package")
        .ok_or_else(|| "no [package] table".to_owned())?;
    let version = package
        .get("version")
        .ok_or_else(|| "the [package] table has no version".to_owned())?;
    let version = version.as_str().ok_or_else(|| {
        "version is not a string (a workspace-inherited version is not checked here)".to_owned()
    })?;
    if version.trim().is_empty() {
        return Err("version is empty".to_owned());
    }
    Ok(version.to_owned())
}

/// Read one member manifest's version, resolving it against the workspace root.
#[cfg(test)]
fn member_version(root: &Path, manifest: &str) -> Result<String, ManifestError> {
    let path = root.join(manifest);
    let display = path.display().to_string();
    let text = std::fs::read_to_string(&path).map_err(|err| ManifestError::Read {
        path: display.clone(),
        reason: err.to_string(),
    })?;
    package_version(&text).map_err(|reason| {
        if reason == "no [package] table" {
            ManifestError::NoPackageTable { path: display }
        } else {
            ManifestError::Parse {
                path: display,
                reason,
            }
        }
    })
}

/// Every workspace member's version, checked against the root package's.
///
/// Returns the disagreements as `(manifest, found, expected)`. An empty result means every member
/// carries the root's version. A member whose manifest cannot be read is returned as a disagreement
/// with the error text as `found`, so an unreadable manifest fails the check instead of passing it.
#[cfg(test)]
pub(crate) fn version_mismatches(root: &Path, expected: &str) -> Vec<(String, String, String)> {
    let mut mismatches = Vec::new();
    // The root manifest's own version comes from the compiled-in constant, so this list holds only
    // the members that are NOT the root package.
    for manifest in WORKSPACE_MEMBERS {
        match member_version(root, manifest) {
            Ok(found) if found == expected => {}
            Ok(found) => mismatches.push(((*manifest).to_owned(), found, expected.to_owned())),
            Err(err) => {
                mismatches.push(((*manifest).to_owned(), err.to_string(), expected.to_owned()))
            }
        }
    }
    mismatches
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads only the \`[package]\` version, not a dependency's or the workspace's.
    #[test]
    fn reads_the_package_version_and_not_a_dependency_version() {
        let manifest = r#"
[workspace]
members = ["finder-ext"]

[package]
name = "oh-my-tab"
version = "0.2.6"
edition = "2021"

[dependencies]
finder-protocol = { path = "crates/finder-protocol" }
serde = { version = "1", features = ["derive"] }

[workspace.package]
version = "9.9.9"
"#;
        assert_eq!(package_version(manifest), Ok("0.2.6".to_owned()));
    }

    /// Counter-example: a dependency table listed BEFORE \`[package]\` must not be mistaken for it.
    #[test]
    fn a_dependency_version_before_the_package_table_is_not_the_package_version() {
        let manifest = r#"
[dependencies]
serde = { version = "1" }

[package]
name = "x"
version = "0.3.0"
"#;
        assert_eq!(package_version(manifest), Ok("0.3.0".to_owned()));
    }

    /// Counter-example (found in review): a version line inside a multi-line string is string
    /// content, not the package version. A line-based scan reads it and reports the wrong version,
    /// which would let a disagreeing tree pass.
    #[test]
    fn a_version_inside_a_multiline_string_is_not_the_package_version() {
        let manifest = r#"
[package]
name = "example"
description = """
version = "0.2.5"
"""
version = "0.2.4"
"#;
        assert_eq!(package_version(manifest), Ok("0.2.4".to_owned()));
    }

    /// Counter-example: a version inside an array or an inline table belongs to that structure, not
    /// to the package.
    #[test]
    fn a_version_inside_a_nested_structure_is_not_the_package_version() {
        let manifest = r#"
[package]
name = "example"
keywords = [
    "version = 9.9.9",
]
metadata = { version = "9.9.9" }
version = "0.4.1"
"#;
        assert_eq!(package_version(manifest), Ok("0.4.1".to_owned()));
    }

    /// Single-quoted TOML literals are valid, so they must read the same way.
    #[test]
    fn accepts_a_single_quoted_version() {
        let manifest = "[package]\nversion = '0.2.6'\n";
        assert_eq!(package_version(manifest), Ok("0.2.6".to_owned()));
    }

    /// Counter-example: an empty or version-less parse must be an error, never an agreement. A
    /// parser that reads nothing would otherwise report "no mismatches" for any tree.
    #[test]
    fn a_manifest_without_a_package_version_is_an_error() {
        assert!(package_version("").is_err());
        assert!(package_version("[workspace]\nmembers = []\n").is_err());
        assert!(package_version("[package]\nname = \"x\"\n").is_err());
        assert!(package_version("[package]\nversion = \"\"\n").is_err());
    }

    /// Counter-example: an inherited version cannot be compared here, so it is reported rather than
    /// silently treated as agreement.
    #[test]
    fn a_workspace_inherited_version_is_refused() {
        assert!(package_version("[package]\nname = \"x\"\nversion.workspace = true\n").is_err());
        assert!(
            package_version("[package]\nname = \"x\"\nversion = { workspace = true }\n").is_err()
        );
    }

    /// Counter-example: malformed TOML is an error, not a silently empty read.
    #[test]
    fn malformed_toml_is_an_error() {
        assert!(package_version("[package]\nname = ").is_err());
    }

    /// The real repository: every workspace member carries the app's version.
    #[test]
    fn workspace_members_share_the_app_version() {
        let root = Path::new(WORKSPACE_ROOT);
        let mismatches = version_mismatches(root, APP_VERSION);
        assert!(
            mismatches.is_empty(),
            "workspace members disagree with the root Cargo.toml (app version {APP_VERSION}): {mismatches:?}\n\
             Bump every member's version together, or the .app ships a bundle whose embedded crates \
             carry another version."
        );
    }

    /// The list must not silently become empty: an empty member list would agree with everything.
    #[test]
    fn the_member_list_is_not_empty() {
        assert!(!WORKSPACE_MEMBERS.is_empty());
    }

    /// Counter-example: every member reported against a wrong expected version means the reader
    /// actually reached all of them; a reader that found nothing would report zero mismatches.
    #[test]
    fn a_wrong_expected_version_reports_every_member() {
        let root = Path::new(WORKSPACE_ROOT);
        let mismatches = version_mismatches(root, "0.0.0-not-a-real-version");
        assert_eq!(
            mismatches.len(),
            WORKSPACE_MEMBERS.len(),
            "a wrong expected version must be reported for every member: {mismatches:?}"
        );
    }
}
