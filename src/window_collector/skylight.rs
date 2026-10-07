//! Window-to-Space membership from SkyLight. The framework is private, so every entry point is
//! resolved at runtime and a missing symbol makes the caller use its legacy policy.

use crate::ffi::{
    CFArrayCreate, CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFDictionaryGetCount,
    CFDictionaryGetKeysAndValues, CFDictionaryGetTypeID, CFDictionaryGetValue, CFGetTypeID,
    CFNumberCreate, CFNumberGetTypeID, CFNumberGetValue, CFRelease, CFStringCreateWithCString,
    CFStringGetTypeID, CFUUIDCreateString, CGDisplayCreateUUIDFromDisplayID, CGMainDisplayID,
};
use crate::log_debug;
use crate::skylight::{load_private_symbol, SKYLIGHT_PATH};
use std::collections::{HashMap, HashSet};
use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

type CGSConnectionID = u32;
type CGSSpaceID = u64;
type CGWindowID = u32;

type CopyManagedDisplaySpacesFn = unsafe extern "C" fn(CGSConnectionID) -> *const c_void;
type ManagedDisplayGetCurrentSpaceFn = unsafe extern "C" fn(CGSConnectionID, *const c_void) -> u64;
type CopyWindowsWithOptionsAndTagsFn = unsafe extern "C" fn(
    CGSConnectionID,
    isize,
    *const c_void,
    isize,
    *mut isize,
    *mut isize,
) -> *const c_void;
type CopySpacesForWindowsFn =
    unsafe extern "C" fn(CGSConnectionID, isize, *const c_void) -> *const c_void;

static CGS_COPY_MANAGED_DISPLAY_SPACES: LazyLock<Option<CopyManagedDisplaySpacesFn>> =
    LazyLock::new(|| unsafe { load_private_symbol(SKYLIGHT_PATH, "CGSCopyManagedDisplaySpaces") });
static CGS_MANAGED_DISPLAY_GET_CURRENT_SPACE: LazyLock<Option<ManagedDisplayGetCurrentSpaceFn>> =
    LazyLock::new(|| unsafe {
        load_private_symbol(SKYLIGHT_PATH, "CGSManagedDisplayGetCurrentSpace")
    });
static CGS_COPY_WINDOWS_WITH_OPTIONS_AND_TAGS: LazyLock<Option<CopyWindowsWithOptionsAndTagsFn>> =
    LazyLock::new(|| unsafe {
        load_private_symbol(SKYLIGHT_PATH, "CGSCopyWindowsWithOptionsAndTags")
    });
static CGS_COPY_SPACES_FOR_WINDOWS: LazyLock<Option<CopySpacesForWindowsFn>> =
    LazyLock::new(|| unsafe { load_private_symbol(SKYLIGHT_PATH, "CGSCopySpacesForWindows") });
static MANAGED_DISPLAY_SHAPE_LOGGED: AtomicBool = AtomicBool::new(false);

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CGS_SPACE_MASK_ALL: isize = 7;
// AltTab's per-Space query enables both invisible tags and ScreenSaver-level windows, then
// backfills any discovered CG window it did not return with CGSCopySpacesForWindows.
const K_CGS_WINDOWS_QUERY_OPTIONS: isize = (1 << 0) | (1 << 1) | (1 << 2);

/// A single WindowServer observation. `current_space_ids` is the union across active displays;
/// each discovered window maps to every Space SkyLight reports for it (possibly none).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct MembershipSnapshot {
    pub(super) current_space_ids: HashSet<CGSSpaceID>,
    pub(super) window_space_ids: HashMap<CGWindowID, Vec<CGSSpaceID>>,
    pub(super) topology: crate::space_groups::Topology,
    pub(super) window_space_type_masks: HashMap<CGWindowID, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MembershipQueryError {
    MissingSymbol,
    MissingConnection,
    InvalidDisplaySnapshot,
    WindowServerQueryFailed,
}

/// Injectable boundary for the membership gate: headless tests provide maps without calling CGS.
pub(super) trait MembershipProvider {
    fn query(&self, window_ids: &[CGWindowID]) -> Result<MembershipSnapshot, MembershipQueryError>;
}

pub(super) struct SkyLightMembershipProvider;

impl MembershipProvider for SkyLightMembershipProvider {
    fn query(&self, window_ids: &[CGWindowID]) -> Result<MembershipSnapshot, MembershipQueryError> {
        query_skylight(window_ids)
    }
}

pub(super) fn query_with_provider(
    provider: &impl MembershipProvider,
    window_ids: &[CGWindowID],
) -> Result<MembershipSnapshot, MembershipQueryError> {
    provider.query(window_ids)
}

fn query_skylight(window_ids: &[CGWindowID]) -> Result<MembershipSnapshot, MembershipQueryError> {
    let connection =
        crate::skylight::cgs_main_connection().ok_or(MembershipQueryError::MissingConnection)?;
    let (Some(copy_displays), Some(current_space), Some(windows_in_space), Some(spaces_for_window)) = (
        *CGS_COPY_MANAGED_DISPLAY_SPACES,
        *CGS_MANAGED_DISPLAY_GET_CURRENT_SPACE,
        *CGS_COPY_WINDOWS_WITH_OPTIONS_AND_TAGS,
        *CGS_COPY_SPACES_FOR_WINDOWS,
    ) else {
        return Err(MembershipQueryError::MissingSymbol);
    };

    let display_spaces = OwnedCf(unsafe { copy_displays(connection) });
    if !has_type(display_spaces.0, unsafe { CFArrayGetTypeID() }) {
        return Err(MembershipQueryError::InvalidDisplaySnapshot);
    }
    log_managed_display_shape_once(display_spaces.0);

    let spaces_key = OwnedCf(cf_string("Spaces")?);
    let display_identifier_key = OwnedCf(cf_string("Display Identifier")?);
    let display_uuid_key = OwnedCf(cf_string("Display UUID")?);
    let space_id_key = OwnedCf(cf_string("id64")?);
    // The native Space kind, independent of any window: measured values are 0 for an ordinary Space
    // and 4 for a fullscreen one. Reading it here means an empty fullscreen Space is still known to
    // be fullscreen, which the window-mask pass below cannot tell.
    let space_type_key = OwnedCf(cf_string("type")?);
    let display_count = unsafe { CFArrayGetCount(display_spaces.0) };
    if display_count <= 0 {
        return Err(MembershipQueryError::InvalidDisplaySnapshot);
    }

    let mut snapshot = MembershipSnapshot::default();
    let mut all_space_ids = HashSet::new();
    for display_index in 0..display_count {
        let display = unsafe { CFArrayGetValueAtIndex(display_spaces.0, display_index) };
        if !has_type(display, unsafe { CFDictionaryGetTypeID() }) {
            return Err(MembershipQueryError::InvalidDisplaySnapshot);
        }
        let identifier =
            cf_string_value(unsafe { CFDictionaryGetValue(display, display_identifier_key.0) });
        let display_uuid =
            cf_string_value(unsafe { CFDictionaryGetValue(display, display_uuid_key.0) });
        let main_uuid = (identifier.as_deref() == Some("Main"))
            .then(main_display_uuid)
            .flatten();
        let Some(display_uuid) = resolve_display_uuid(
            identifier.as_deref(),
            display_uuid.as_deref(),
            main_uuid.as_deref(),
        ) else {
            return Err(MembershipQueryError::InvalidDisplaySnapshot);
        };
        let display_uuid_value = display_uuid.to_string();
        let display_uuid = OwnedCf(cf_string(display_uuid)?);
        let current = unsafe { current_space(connection, display_uuid.0) };
        if current == 0 {
            return Err(MembershipQueryError::InvalidDisplaySnapshot);
        }
        snapshot.current_space_ids.insert(current);
        let display_spaces = snapshot
            .topology
            .displays
            .entry(display_uuid_value)
            .or_default();
        display_spaces.current = current;

        let spaces = unsafe { CFDictionaryGetValue(display, spaces_key.0) };
        if !has_type(spaces, unsafe { CFArrayGetTypeID() }) {
            return Err(MembershipQueryError::InvalidDisplaySnapshot);
        }
        for space_index in 0..unsafe { CFArrayGetCount(spaces) } {
            let space = unsafe { CFArrayGetValueAtIndex(spaces, space_index) };
            if !has_type(space, unsafe { CFDictionaryGetTypeID() }) {
                return Err(MembershipQueryError::InvalidDisplaySnapshot);
            }
            let raw_id = unsafe { CFDictionaryGetValue(space, space_id_key.0) };
            let space_id = unsafe { cf_number_u64(raw_id) }
                .ok_or(MembershipQueryError::InvalidDisplaySnapshot)?;
            if space_id == 0 {
                return Err(MembershipQueryError::InvalidDisplaySnapshot);
            }
            all_space_ids.insert(space_id);
            // The native index order is what `DisplaySpaces::inferred_origin` reads: macOS keeps a
            // fullscreen Space right after the ordinary Space it was created from.
            display_spaces.ordered.push(space_id);
            let native_kind =
                match unsafe { cf_number_u64(CFDictionaryGetValue(space, space_type_key.0)) } {
                    Some(0) => crate::space_groups::SpaceKind::Ordinary,
                    Some(4) => crate::space_groups::SpaceKind::Fullscreen,
                    _ => crate::space_groups::SpaceKind::Unknown,
                };
            display_spaces.spaces.entry(space_id).or_insert(native_kind);
        }
    }
    if snapshot.current_space_ids.is_empty()
        || all_space_ids.is_empty()
        || snapshot
            .current_space_ids
            .iter()
            .any(|current| !all_space_ids.contains(current))
    {
        return Err(MembershipQueryError::InvalidDisplaySnapshot);
    }

    let requested: HashSet<CGWindowID> = window_ids.iter().copied().filter(|id| *id != 0).collect();
    snapshot
        .window_space_ids
        .extend(requested.iter().map(|window_id| (*window_id, Vec::new())));

    // Invert AltTab's per-Space fan-out: one WindowServer query per managed Space, rather than
    // one IPC per discovered window. Some order-out windows are absent from this enumeration;
    // the targeted per-window call below backfills those IDs just as AltTab does.
    let mut all_space_ids: Vec<_> = all_space_ids.into_iter().collect();
    all_space_ids.sort_unstable();
    for space_id in all_space_ids {
        let (space_array, _space_number) = cf_number_array_i64(space_id as i64)?;
        let mut set_tags = 0isize;
        let mut clear_tags = 0isize;
        let listed_windows = OwnedCf(unsafe {
            windows_in_space(
                connection,
                0,
                space_array.0,
                K_CGS_WINDOWS_QUERY_OPTIONS,
                &mut set_tags,
                &mut clear_tags,
            )
        });
        if !has_type(listed_windows.0, unsafe { CFArrayGetTypeID() }) {
            return Err(MembershipQueryError::WindowServerQueryFailed);
        }
        for index in 0..unsafe { CFArrayGetCount(listed_windows.0) } {
            let value = unsafe { CFArrayGetValueAtIndex(listed_windows.0, index) };
            let Some(window_id) =
                (unsafe { cf_number_u64(value) }).and_then(|id| u32::try_from(id).ok())
            else {
                return Err(MembershipQueryError::WindowServerQueryFailed);
            };
            if requested.contains(&window_id) {
                let memberships = snapshot.window_space_ids.entry(window_id).or_default();
                if !memberships.contains(&space_id) {
                    memberships.push(space_id);
                }
            }
        }
    }

    for window_id in requested {
        if snapshot
            .window_space_ids
            .get(&window_id)
            .is_some_and(|spaces| !spaces.is_empty())
        {
            continue;
        }
        let (window_array, _window_number) = cf_number_array_i64(window_id as i64)?;
        let spaces =
            OwnedCf(unsafe { spaces_for_window(connection, K_CGS_SPACE_MASK_ALL, window_array.0) });
        if !has_type(spaces.0, unsafe { CFArrayGetTypeID() }) {
            return Err(MembershipQueryError::WindowServerQueryFailed);
        }
        let mut memberships = Vec::new();
        for index in 0..unsafe { CFArrayGetCount(spaces.0) } {
            let value = unsafe { CFArrayGetValueAtIndex(spaces.0, index) };
            let Some(space_id) = (unsafe { cf_number_u64(value) }) else {
                return Err(MembershipQueryError::WindowServerQueryFailed);
            };
            memberships.push(space_id);
        }
        snapshot.window_space_ids.insert(window_id, memberships);
    }

    if let Some(masks) = crate::skylight::window_space_type_masks(window_ids) {
        for (window_id, mask) in masks {
            snapshot.window_space_type_masks.insert(window_id, mask);
            let kind = if mask & 0x20 != 0 {
                crate::space_groups::SpaceKind::Fullscreen
            } else if mask & 0x1 != 0 {
                crate::space_groups::SpaceKind::Ordinary
            } else {
                crate::space_groups::SpaceKind::Unknown
            };
            if kind != crate::space_groups::SpaceKind::Unknown {
                if let Some(spaces) = snapshot.window_space_ids.get(&window_id) {
                    for space_id in spaces {
                        for display in snapshot.topology.displays.values_mut() {
                            if let Some(space_kind) = display.spaces.get_mut(space_id) {
                                if *space_kind != crate::space_groups::SpaceKind::Fullscreen
                                    || kind == crate::space_groups::SpaceKind::Fullscreen
                                {
                                    *space_kind = kind;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(snapshot)
}

fn log_managed_display_shape_once(display_spaces: *const c_void) {
    if MANAGED_DISPLAY_SHAPE_LOGGED.swap(true, Ordering::Relaxed) {
        return;
    }
    let identifier_key = match cf_string("Display Identifier") {
        Ok(key) => OwnedCf(key),
        Err(_) => return,
    };
    let uuid_key = match cf_string("Display UUID") {
        Ok(key) => OwnedCf(key),
        Err(_) => return,
    };
    let cg_main_display_uuid = main_display_uuid();
    let count = unsafe { CFArrayGetCount(display_spaces) };
    for index in 0..count {
        let display = unsafe { CFArrayGetValueAtIndex(display_spaces, index) };
        if !has_type(display, unsafe { CFDictionaryGetTypeID() }) {
            log_debug!(
                "[collect] SkyLight display mapping probe: display={} invalid_entry",
                index
            );
            continue;
        }
        let entry_count = unsafe { CFDictionaryGetCount(display) }.max(0) as usize;
        let mut keys = vec![std::ptr::null(); entry_count];
        let mut values = vec![std::ptr::null(); entry_count];
        unsafe { CFDictionaryGetKeysAndValues(display, keys.as_mut_ptr(), values.as_mut_ptr()) };
        let key_names = keys
            .iter()
            .filter_map(|key| cf_string_value(*key))
            .collect::<Vec<_>>();
        let identifier = unsafe { CFDictionaryGetValue(display, identifier_key.0) };
        let display_uuid = unsafe { CFDictionaryGetValue(display, uuid_key.0) };
        log_debug!(
            "[collect] SkyLight display mapping probe: display={} keys={:?} display_uuid={:?} display_identifier={:?} cg_main_display_uuid={:?}",
            index,
            key_names,
            cf_string_value(display_uuid),
            cf_string_value(identifier),
            cg_main_display_uuid
        );
    }
}

fn cf_string_value(value: *const c_void) -> Option<String> {
    has_type(value, unsafe { CFStringGetTypeID() })
        .then(|| crate::window_collector::cf_to_rust_string(value))
        .flatten()
}

fn resolve_display_uuid<'a>(
    identifier: Option<&'a str>,
    dictionary_uuid: Option<&'a str>,
    main_display_uuid: Option<&'a str>,
) -> Option<&'a str> {
    if identifier == Some("Main") {
        dictionary_uuid.or(main_display_uuid)
    } else {
        dictionary_uuid.or(identifier.filter(|value| !value.is_empty()))
    }
}

fn main_display_uuid() -> Option<String> {
    let uuid = unsafe { CGDisplayCreateUUIDFromDisplayID(CGMainDisplayID()) };
    if uuid.is_null() {
        return None;
    }
    let string = OwnedCf(unsafe { CFUUIDCreateString(std::ptr::null(), uuid) });
    unsafe { CFRelease(uuid) };
    cf_string_value(string.0)
}

fn cf_string(value: &str) -> Result<*const c_void, MembershipQueryError> {
    let Ok(value) = CString::new(value) else {
        return Err(MembershipQueryError::InvalidDisplaySnapshot);
    };
    let string = unsafe {
        CFStringCreateWithCString(std::ptr::null(), value.as_ptr(), K_CF_STRING_ENCODING_UTF8)
    };
    if string.is_null() {
        Err(MembershipQueryError::InvalidDisplaySnapshot)
    } else {
        Ok(string)
    }
}

fn has_type(value: *const c_void, expected: usize) -> bool {
    !value.is_null() && unsafe { CFGetTypeID(value) == expected }
}

fn cf_number_array_i64(value: i64) -> Result<(OwnedCf, OwnedCf), MembershipQueryError> {
    let number =
        OwnedCf(unsafe { CFNumberCreate(std::ptr::null(), 4, (&value as *const i64).cast()) });
    if number.0.is_null() {
        return Err(MembershipQueryError::WindowServerQueryFailed);
    }
    let values = [number.0];
    let array = OwnedCf(unsafe {
        CFArrayCreate(
            std::ptr::null(),
            values.as_ptr(),
            values.len() as isize,
            std::ptr::null(),
        )
    });
    if array.0.is_null() {
        return Err(MembershipQueryError::WindowServerQueryFailed);
    }
    Ok((array, number))
}

unsafe fn cf_number_u64(number: *const c_void) -> Option<u64> {
    if !has_type(number, CFNumberGetTypeID()) {
        return None;
    }
    let mut value = 0i64;
    (CFNumberGetValue(number, 4, (&mut value as *mut i64).cast()) && value >= 0)
        .then_some(value as u64)
}

/// Owns a CF object and releases it on every exit path. Shared with the AX batch read in
/// `raiser`, whose slots are caller-owned and whose early returns used to leak them.
pub(super) struct OwnedCf(pub(super) *const c_void);

impl Drop for OwnedCf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider(MembershipSnapshot);

    impl MembershipProvider for FakeProvider {
        fn query(
            &self,
            _window_ids: &[CGWindowID],
        ) -> Result<MembershipSnapshot, MembershipQueryError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn membership_provider_is_injectable_without_calling_skylight() {
        let expected = MembershipSnapshot {
            current_space_ids: HashSet::from([10, 20]),
            window_space_ids: HashMap::from([(7, vec![20]), (8, vec![30])]),
            ..Default::default()
        };
        let queried = query_with_provider(&FakeProvider(expected.clone()), &[7, 8]).unwrap();
        assert_eq!(queried, expected);
        assert!(queried.window_space_ids.contains_key(&7));
        assert!(!queried.window_space_ids[&7].is_empty());
        assert!(queried.window_space_ids[&8]
            .iter()
            .all(|space| !queried.current_space_ids.contains(space)));
    }

    #[test]
    fn display_identifier_resolves_to_the_uuid_used_by_skylight() {
        assert_eq!(
            resolve_display_uuid(Some("Main"), None, Some("main-uuid")),
            Some("main-uuid")
        );
        assert_eq!(
            resolve_display_uuid(Some("Main"), Some("dict-main"), Some("cg-main")),
            Some("dict-main")
        );
        assert_eq!(
            resolve_display_uuid(Some("display-uuid"), None, Some("main-uuid")),
            Some("display-uuid")
        );
        assert_eq!(
            resolve_display_uuid(Some("display-id"), Some("dict-uuid"), None),
            Some("dict-uuid")
        );
        assert_eq!(resolve_display_uuid(None, None, Some("main-uuid")), None);
    }
}
