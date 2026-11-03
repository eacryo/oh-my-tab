//! One settings page as data: its rows, their values and their identities.
//!
//! A page module is pure: it reads `PageInput`, writes `RowSpec`s through `RowWriter` and returns
//! a `PageView`. It creates no view, reads no global and records no search index -- the shared
//! renderer does all of that from the description.

use super::*;

/// The Finder Menu page's description: its rows, their values and their identities.
pub(super) fn finder_menu_page_view(_input: &PageInput) -> PageView {
    unimplemented!("Finder Menu is not migrated to a page description yet")
}

/// What the Finder Menu page draws itself: its legacy `SettingsUi` pointers are not in the shared
/// slot table, so the page binds them after its build.
#[derive(Default)]
pub(crate) struct FinderMenuHooks {}

impl PageHooks for FinderMenuHooks {
    unsafe fn after_page(
        &mut self,
        _canvas: &mut PageCanvas,
        _rendered: &mut RenderedPage,
        _ui: &mut SettingsUi,
        _target: *mut AnyObject,
    ) -> Result<(), RegisterError> {
        Ok(())
    }
}
