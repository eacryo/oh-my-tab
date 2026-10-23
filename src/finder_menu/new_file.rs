//! Creating a new file in a Finder folder.
//!
//! The name is localized and collision-safe: the first attempt is "Untitled.<ext>", then
//! "Untitled 2.<ext>" and so on. Creation uses O_EXCL, so a name that appears between the check and
//! the write is retried instead of overwriting the existing file.

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::i18n::t;

/// Create an empty file of `extension` inside `directory`. The caller has already verified the
/// directory and that the extension is one the config offers.
pub(crate) fn create_in(directory: &Path, extension: &str) -> std::io::Result<PathBuf> {
    if !directory.is_dir() {
        return Err(std::io::Error::new(
            ErrorKind::NotFound,
            "target directory does not exist",
        ));
    }
    let base = t("finder_menu.untitled");
    // Finder's own numbering: "Untitled.md", then "Untitled 2.md", "Untitled 3.md" (it never
    // creates "Untitled 1").
    for suffix in 0..1000u32 {
        let name = if suffix == 0 {
            format!("{base}.{extension}")
        } else {
            format!("{base} {}.{extension}", suffix + 1)
        };
        let path = directory.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                // An empty file is the whole template for now; user templates are a later feature.
                file.flush()?;
                return Ok(path);
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        ErrorKind::AlreadyExists,
        "too many files named like the new file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_file_is_untitled_with_the_extension() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = create_in(directory.path(), "md").expect("create");
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{}.md", t("finder_menu.untitled"))
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[test]
    fn collisions_get_a_number_suffix() {
        let directory = tempfile::tempdir().expect("tempdir");
        let base = t("finder_menu.untitled");
        std::fs::write(directory.path().join(format!("{base}.md")), b"existing").unwrap();
        let path = create_in(directory.path(), "md").expect("create");
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{base} 2.md")
        );
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let missing = directory.path().join("missing");
        assert!(create_in(&missing, "md").is_err());
    }
}
