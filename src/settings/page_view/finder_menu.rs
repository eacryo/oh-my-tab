//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns a
//! `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.

use super::*;

/// The Finder Menu page's description: its rows, their values and their identities.
pub(super) fn finder_menu_page_view(_input: &PageInput) -> PageView {
    unimplemented!("Finder Menu is not migrated to a page description yet")
}
