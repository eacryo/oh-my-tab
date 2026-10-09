//! Clipboard subsystem · keyring: where the master key comes from.
//!
//! The key is a random 32-byte value plus a random 16-byte id, stored as one login-keychain
//! generic-password item. Every build channel (dev and release) reads the SAME item, because
//! they share the history file: separate keys would make each channel unable to read the
//! other's history. Authorization is the ACL's business, and it is judged against the app that
//! CREATED the item: an item created by a self-signed or ad-hoc build stops being recognized as soon
//! as that binary changes (measured: rebuild + re-sign, then a prompt), while one created by a
//! Developer ID-signed build survives the same rebuild (measured: silence, twice). An app cannot
//! adopt an item it did not create (`errSecInvalidOwnerEdit`), so the fix for an install whose item
//! predates Developer ID signing is a one-time reset -- see `keyring_acl` and the plan's §6/§7.
//!
//! Creation is **create-only**: `ItemAddOptions::add()` wraps `SecItemAdd`, which fails with
//! `errSecDuplicateItem` instead of updating. `passwords::set_generic_password` is
//! create-OR-update and would overwrite the other channel's key, destroying its history.
//!
//! Every call here blocks (and may block on a system authorization prompt), so callers must be
//! on the clipboard persistence worker, never the main thread.

use super::*;
use core_foundation::data::CFData;
use security_framework::item::{
    ItemAddOptions, ItemAddValue, ItemClass, ItemSearchOptions, SearchResult,
};
use security_framework_sys::base::{errSecAuthFailed, errSecDuplicateItem, errSecItemNotFound};

/// The keychain item holding the clipboard master key (service + account).
pub(super) const KEYCHAIN_SERVICE: &str = "com.eacryo.oh-my-tab.clipboard-history";
const KEYCHAIN_ACCOUNT: &str = "master-key";

/// `errSecUserCanceled` (SecBase.h): the user dismissed the keychain authorization prompt.
const ERR_SEC_USER_CANCELED: i32 = -128;
/// `errSecInteractionNotAllowed` (SecBase.h): the keychain is locked / interaction is refused.
const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

/// The key used by `cfg(test)` and `--smoke-*` runs. Those runs keep every file under a
/// temporary directory, so this key protects nothing but test data; it is not a secret and is
/// never used by a real build (the backend below is chosen at runtime, and only test/smoke
/// modes can select it).
const TEST_KEY_ID: [u8; KEY_ID_LEN] = *b"test-key-id-0001";
const TEST_KEY: [u8; KEY_LEN] = *b"not-a-secret-test-key-0123456789";

/// Where the master key comes from in this process.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum KeyBackend {
    Keychain,
    TestKey,
    /// `--clip-no-keychain`: simulate an unavailable keychain (the degraded path).
    SimulatedUnavailable,
}

/// Why no key is available. The storage layer maps this to `Unavailable` (never to a plaintext
/// write, and never to replacing an existing key).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum KeyUnavailable {
    /// `--clip-no-keychain` (development switch for the degraded path).
    Simulated,
    /// The keychain item does not exist.
    Missing,
    /// The item exists but could not be read: refused, cancelled, or the keychain is locked.
    AccessDenied,
    /// Anything else (a foreign item with the wrong shape, an unexpected platform error).
    Other,
}

/// Which backend this process uses. `--clip-no-keychain` wins even in smoke mode, so the
/// degraded path stays reachable from a smoke runner.
pub(super) fn current_backend() -> KeyBackend {
    if crate::dev_flags::present("clip-no-keychain") {
        return KeyBackend::SimulatedUnavailable;
    }
    if cfg!(test) || SMOKE_MODE.load(Ordering::SeqCst) {
        return KeyBackend::TestKey;
    }
    KeyBackend::Keychain
}

/// A short label for logs and `--e2e-state` (never the key material).
pub(super) fn backend_label(backend: KeyBackend) -> &'static str {
    match backend {
        KeyBackend::Keychain => "keychain",
        KeyBackend::TestKey => "test",
        KeyBackend::SimulatedUnavailable => "none",
    }
}

/// A short label for a failure reason (logs and `--e2e-state`).
pub(super) fn unavailable_label(reason: &KeyUnavailable) -> &'static str {
    match reason {
        KeyUnavailable::Simulated => "simulated",
        KeyUnavailable::Missing => "missing",
        KeyUnavailable::AccessDenied => "access-denied",
        KeyUnavailable::Other => "other",
    }
}

/// Pure: a keychain status code -> the failure classification. Kept separate from the calls so
/// the mapping is unit-testable without a keychain.
pub(super) fn classify_keychain_error(code: i32) -> KeyUnavailable {
    if code == errSecItemNotFound {
        KeyUnavailable::Missing
    } else if code == errSecAuthFailed
        || code == ERR_SEC_USER_CANCELED
        || code == ERR_SEC_INTERACTION_NOT_ALLOWED
    {
        KeyUnavailable::AccessDenied
    } else {
        KeyUnavailable::Other
    }
}

/// Obtain the master key for this process.
///
/// `allow_create` is the storage layer's decision (invariant R1): a key is only created when no
/// encrypted data exists, or after an explicit purge — never while readable-but-unopenable
/// data is on disk. A duplicate item is re-read, never updated.
pub(super) fn acquire(allow_create: bool) -> Result<MasterKey, KeyUnavailable> {
    match current_backend() {
        KeyBackend::SimulatedUnavailable => Err(KeyUnavailable::Simulated),
        KeyBackend::TestKey => Ok(test_key()),
        KeyBackend::Keychain => acquire_from_keychain(allow_create),
    }
}

/// The fixed test key (see TEST_KEY's comment).
pub(super) fn test_key() -> MasterKey {
    MasterKey::from_item_value(&[TEST_KEY_ID.as_slice(), TEST_KEY.as_slice()].concat())
        .expect("the test key literal has the right length")
}

fn acquire_from_keychain(allow_create: bool) -> Result<MasterKey, KeyUnavailable> {
    // One ACL read per load, on this worker: `--e2e-state` then reports it without touching the
    // keychain from the main thread. The answer says whether this build can read the item silently;
    // see `keyring_acl` for why a development build usually cannot.
    log_debug!(
        "[clip] keychain ACL identity: {}",
        super::keyring_acl::refresh_acl_identity().label()
    );
    match read_item() {
        Ok(key) => Ok(key),
        Err(KeyUnavailable::Missing) if allow_create => match create_item() {
            Ok(key) => Ok(key),
            // Lost the race against the other channel: it created the item between our read and
            // our add. Read it instead of insisting on ours.
            Err(KeyUnavailable::Other) => read_item().map_err(|_| KeyUnavailable::Other),
            Err(other) => Err(other),
        },
        Err(other) => Err(other),
    }
}

/// Read the key item. Never creates, never updates.
fn read_item() -> Result<MasterKey, KeyUnavailable> {
    let mut options = ItemSearchOptions::new();
    options
        .class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE)
        .account(KEYCHAIN_ACCOUNT)
        .load_data(true)
        .limit(1);
    let results = options
        .search()
        .map_err(|e| classify_keychain_error(e.code()))?;
    let bytes = results.into_iter().find_map(|result| match result {
        SearchResult::Data(data) => Some(data),
        _ => None,
    });
    let Some(bytes) = bytes else {
        return Err(KeyUnavailable::Missing);
    };
    // A foreign item under our service/account (wrong length or malformed) is not "missing":
    // creating a key next to it would split the storage set across two keys.
    MasterKey::from_item_value(&bytes).ok_or(KeyUnavailable::Other)
}

/// Create the key item (create-only). `errSecDuplicateItem` means someone else got there first.
fn create_item() -> Result<MasterKey, KeyUnavailable> {
    let key = MasterKey::generate().ok_or(KeyUnavailable::Other)?;
    let value = key.to_item_value();
    let mut options = ItemAddOptions::new(ItemAddValue::Data {
        class: ItemClass::generic_password(),
        data: CFData::from_buffer(&value),
    });
    options.set_service(KEYCHAIN_SERVICE);
    options.set_account_name(KEYCHAIN_ACCOUNT);
    match options.add() {
        Ok(()) => Ok(key),
        Err(error) => {
            let code = error.code();
            if code == errSecDuplicateItem {
                // Deliberately classified as Other so the caller re-reads instead of trusting
                // this error as a reason to give up.
                log_debug!("[clip] keychain item already exists; reading the existing key");
                Err(KeyUnavailable::Other)
            } else {
                Err(classify_keychain_error(code))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keychain_errors_map_to_the_failure_classification() {
        assert_eq!(
            classify_keychain_error(errSecItemNotFound),
            KeyUnavailable::Missing
        );
        for code in [
            errSecAuthFailed,
            ERR_SEC_USER_CANCELED,
            ERR_SEC_INTERACTION_NOT_ALLOWED,
        ] {
            assert_eq!(
                classify_keychain_error(code),
                KeyUnavailable::AccessDenied,
                "code {code}"
            );
        }
        // A duplicate is deliberately NOT Missing: the caller must re-read the existing item.
        assert_eq!(
            classify_keychain_error(errSecDuplicateItem),
            KeyUnavailable::Other
        );
        assert_eq!(classify_keychain_error(-1), KeyUnavailable::Other);
    }

    #[test]
    fn the_test_key_has_the_envelope_shape() {
        let key = test_key();
        assert_eq!(key.to_item_value().len(), KEY_ID_LEN + KEY_LEN);
        assert_eq!(key.key_id(), &TEST_KEY_ID);
    }

    #[test]
    fn the_test_backend_is_selected_under_cfg_test() {
        // Guards the injection: a test run must never reach the real keychain.
        assert_eq!(current_backend(), KeyBackend::TestKey);
        assert!(acquire(false).is_ok());
    }

    #[test]
    fn the_test_key_is_bound_to_test_storage() {
        // The test key must only ever protect temporary storage: the history path in a test
        // build lives under the temp root, never in the user's config directory.
        let path = super::super::persist::history_file_path();
        assert!(
            path.to_string_lossy()
                .contains("oh-my-tab-clip-images-test-"),
            "test history path must be the temporary one, got {}",
            path.display()
        );
    }
}
