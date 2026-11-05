//! The identity table: which control each pointer is, and the page-scoped buffer that fills it.
//!
//! A page registers the controls it builds into `PageRegistrations`, and `publish` moves them into
//! the shared `ControlTable` only once the whole page built successfully -- a page that fails
//! half-way keeps the previous mapping intact.

use super::super::*;
use super::*;
/// One control's registration: the pointer the dispatcher sees, and what it identifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ControlEntry {
    pub(super) pointer: usize,
    pub(super) id: ControlId,
}

/// Why registering a control was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::settings) enum RegisterError {
    /// The same identity is already registered.
    DuplicateId,
    /// The same pointer is already registered.
    DuplicatePointer,
}

/// The identity table: which control pointer edits which field, filled while the page is built.
///
/// Lifecycle, all of it asserted below:
/// - register and unregister in pairs; rebuilding a page unregisters its entries before
///   registering again;
/// - a duplicate registration, of the same identity or of the same pointer, is REFUSED here and
///   aborts that page's build (the caller discards the registrations it made and leaves the
///   previous page alone);
/// - a hit on a destroyed control's address therefore cannot be answered by a stale entry,
///   because the entry is gone before the view is released.
#[derive(Debug, Default)]
pub(in crate::settings) struct ControlTable {
    entries: Vec<ControlEntry>,
}

/// A page's registrations, applied to the shared table only once the whole page is built.
///
/// This is what makes a failed build harmless: the controls exist in the page's view during the
/// build, but the mapping the dispatcher reads is untouched until `commit`. A page that fails
/// half-way drops every registration it made here, so the mapping and the views it names stay
/// together.
#[derive(Debug, Default)]
pub(in crate::settings) struct PageRegistrations {
    entries: Vec<ControlEntry>,
}

impl ControlTable {
    pub(in crate::settings) fn new() -> Self {
        Self::default()
    }

    /// Register one control. The identity is unique across the table, and so is the pointer.
    pub(in crate::settings) fn register(
        &mut self,
        pointer: *mut AnyObject,
        id: ControlId,
    ) -> Result<(), RegisterError> {
        let pointer = pointer as usize;
        if pointer == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(RegisterError::DuplicateId);
        }
        if self.entries.iter().any(|entry| entry.pointer == pointer) {
            return Err(RegisterError::DuplicatePointer);
        }
        self.entries.push(ControlEntry { pointer, id });
        Ok(())
    }

    /// Remove one registration. Returns whether an entry was removed; repeated cleanup is safe.
    pub(in crate::settings) fn unregister(&mut self, id: &ControlId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| &entry.id != id);
        self.entries.len() != before
    }

    /// The identity behind a sender pointer (`None` = an unknown sender: the caller keeps its own
    /// fallback, nothing is silently dropped).
    pub(in crate::settings) fn lookup(&self, pointer: *mut AnyObject) -> Option<ControlId> {
        let pointer = pointer as usize;
        self.entries
            .iter()
            .find(|entry| entry.pointer == pointer)
            .map(|entry| entry.id.clone())
    }

    /// Whether the identity is registered (publish validates against it; the lifecycle tests read
    /// it too).
    pub(in crate::settings) fn contains_id(&self, id: &ControlId) -> bool {
        self.entries.iter().any(|entry| &entry.id == id)
    }

    #[cfg(test)]
    pub(in crate::settings) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(in crate::settings) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every registration: the whole settings window is being torn down.
    pub(in crate::settings) fn clear(&mut self) {
        self.entries.clear();
    }
}

impl PageRegistrations {
    pub(super) fn register(
        &mut self,
        pointer: *mut AnyObject,
        id: ControlId,
    ) -> Result<(), RegisterError> {
        let pointer = pointer as usize;
        if pointer == 0 {
            return Ok(());
        }
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(RegisterError::DuplicateId);
        }
        if self.entries.iter().any(|entry| entry.pointer == pointer) {
            return Err(RegisterError::DuplicatePointer);
        }
        self.entries.push(ControlEntry { pointer, id });
        Ok(())
    }

    /// Publish the page's registrations, replacing whatever it registered before.
    ///
    /// The page's old entries are removed FIRST (so a control released right after this cannot be
    /// reached through a stale pointer), then the new ones are added. On a refusal every entry this
    /// call added is removed again, so the table keeps the previous mapping exactly as it was —
    /// the page's views and its mapping either both change or neither does.
    pub(super) fn publish(&mut self, table: &mut ControlTable) -> Result<(), RegisterError> {
        let new_entries = std::mem::take(&mut self.entries);
        // Check FIRST, against the mapping exactly as it will be after the page's own old entries
        // are gone: a page that cannot be published must not have disturbed anything, so the
        // previous controls and the previous mapping stay together.
        let mut conflict: Option<RegisterError> = None;
        for entry in &new_entries {
            let collides = table.entries.iter().any(|live| {
                let replaced = new_entries.iter().any(|new| new.id == live.id);
                !replaced && (live.id == entry.id || live.pointer == entry.pointer)
            });
            if collides {
                conflict = Some(if table.contains_id(&entry.id) {
                    RegisterError::DuplicateId
                } else {
                    RegisterError::DuplicatePointer
                });
                break;
            }
        }
        if let Some(conflict) = conflict {
            self.entries = new_entries;
            return Err(conflict);
        }
        for entry in &new_entries {
            table.unregister(&entry.id);
        }
        let mut added: Vec<ControlId> = Vec::with_capacity(new_entries.len());
        for entry in &new_entries {
            if let Err(error) = table.register(entry.pointer as *mut AnyObject, entry.id.clone()) {
                for id in &added {
                    table.unregister(id);
                }
                self.entries = new_entries;
                return Err(error);
            }
            added.push(entry.id.clone());
        }
        Ok(())
    }
}
