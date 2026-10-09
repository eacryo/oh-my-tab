//! Clipboard subsystem · keyring_acl: what the keychain item's ACL says about this build.
//!
//! Whether a read of the clipboard master key is silent or asks for the keychain password is decided
//! by the item's ACL, and the ACL is judged against the **designated requirement of the app that
//! created the item**. Everything below is measured (2026-10-08, macOS 27, C probes plus real runs):
//!
//! - An item created by a **self-signed or ad-hoc** build stops being recognized as soon as that
//!   binary changes: a probe created an item, read it silently, then rebuilt and re-signed the same
//!   path -- and the next read prompted. That is why this project's development build asked for the
//!   password over and over.
//! - An item created by a **Developer ID-signed** build survives exactly the same rebuild: the probe
//!   read it silently again, and so does the app (a rebuilt Developer ID dev bundle read the item in
//!   67 ms with no prompt).
//! - The entry's *textual form* does not predict this: macOS reports both as a path through the
//!   deprecated `SecTrustedApplicationCopyData`, so a `path` classification below means "not by
//!   requirement", not "unstable".
//! - An app cannot adopt an item it did not create: `SecItemDelete` on an item created by a different
//!   signature returns `errSecInvalidOwnerEdit` (-25244, measured). Deleting it once -- by the user --
//!   and letting the Developer ID-signed app create a fresh one is the only way out; the app then
//!   quarantines the old storage set and starts clean.
//!
//! So: the diagnostic below reports which form the ACL uses (`--e2e-state`'s `keychain_acl`), and the
//! durable fix for an install whose item predates Developer ID signing is the one-time reset.
//!
//! Reads only ACL metadata, which needs no authorization (measured: probes read it without a prompt).
//! Every call here blocks, so callers must be on the clipboard worker, never the main thread.

use super::keyring::KEYCHAIN_SERVICE;
use core_foundation::base::TCFType;
use security_framework_sys::item::{
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecReturnRef,
};
use std::ffi::{c_char, c_void};

/// The account half of the keychain item (same item as `keyring`).
const KEYCHAIN_ACCOUNT: &str = "master-key";

/// `kSecCSDefaultFlags`.
const SEC_CS_DEFAULT_FLAGS: u32 = 0;
/// `kCFStringEncodingUTF8`.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

type CFStringRef = *const c_void;
type CFDataRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFArrayRef = *const c_void;
type SecKeychainItemRef = *mut c_void;
type SecAccessRef = *mut c_void;
type SecTrustedApplicationRef = *mut c_void;
type SecACLRef = *mut c_void;
type SecRequirementRef = *mut c_void;
type SecCodeRef = *mut c_void;

/// `CSSM_ACL_KEYCHAIN_PROMPT_SELECTOR`, only ever passed as a default-initialized out-parameter.
#[repr(C)]
#[derive(Default)]
struct CssmAclKeychainPromptSelector {
    version: u16,
    flags: u16,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        s: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const CFStringRef,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "Security", kind = "framework")]
extern "C" {
    fn SecKeychainItemCopyAccess(item: SecKeychainItemRef, access: *mut SecAccessRef) -> i32;
    fn SecAccessCopyACLList(access: SecAccessRef, acl_list: *mut CFArrayRef) -> i32;
    fn SecTrustedApplicationCopyData(app: SecTrustedApplicationRef, data: *mut CFDataRef) -> i32;
    fn SecACLCopySimpleContents(
        acl: SecACLRef,
        application_list: *mut CFArrayRef,
        description: *mut CFStringRef,
        prompt_selector: *mut CssmAclKeychainPromptSelector,
    ) -> i32;
    fn SecRequirementCreateWithData(
        data: CFDataRef,
        flags: u32,
        req: *mut SecRequirementRef,
    ) -> i32;
    fn SecCodeCopySelf(flags: u32, code: *mut SecCodeRef) -> i32;
    fn SecCodeCheckValidity(code: SecCodeRef, flags: u32, req: SecRequirementRef) -> i32;
    fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut *const c_void) -> i32;
}

/// How the item's ACL identifies its trusted applications, as far as the deprecated reading API can
/// tell. `--e2e-state` reports it so a scenario can see the shape of the item's ACL.
///
/// NOT a stability verdict: macOS reports a path for every app that is not Apple-anchored, and the
/// measurements in this module's header show that what actually decides whether a rebuild keeps
/// working is the *creating* app's signing identity, not the form reported here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AclIdentity {
    /// At least one trusted application is a requirement this build satisfies.
    Requirement,
    /// The entries are requirements, but none admits this build (another channel's build, or a
    /// certificate that has since been replaced).
    ForeignRequirement,
    /// The ACL identifies applications by path (or by a hash of an older build).
    Path,
    /// No trusted application at all.
    None,
    /// No item yet, or the ACL could not be read.
    Unknown,
}

/// The last observed ACL state. Every keychain call belongs on the clipboard worker (it can block on
/// a password dialog), so `--e2e-state` reads this cache instead of asking the keychain on the main
/// thread.
static LAST_ACL_IDENTITY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// The cached answer, `Unknown` until the load worker has looked once.
pub(super) fn cached_acl_identity() -> AclIdentity {
    match LAST_ACL_IDENTITY.load(std::sync::atomic::Ordering::Relaxed) {
        1 => AclIdentity::Requirement,
        2 => AclIdentity::Path,
        3 => AclIdentity::None,
        4 => AclIdentity::ForeignRequirement,
        _ => AclIdentity::Unknown,
    }
}

/// Read the item's ACL and cache the answer. Call on the clipboard worker only.
pub(super) fn refresh_acl_identity() -> AclIdentity {
    let identity = item_acl_identity();
    let code = match identity {
        AclIdentity::Requirement => 1,
        AclIdentity::Path => 2,
        AclIdentity::None => 3,
        AclIdentity::ForeignRequirement => 4,
        AclIdentity::Unknown => 0,
    };
    LAST_ACL_IDENTITY.store(code, std::sync::atomic::Ordering::Relaxed);
    identity
}

impl AclIdentity {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Requirement => "requirement",
            Self::ForeignRequirement => "foreign-requirement",
            Self::Path => "path",
            Self::None => "none",
            Self::Unknown => "unknown",
        }
    }
}

/// A CF string from a Rust literal; the caller releases it.
unsafe fn cf_string(value: &str) -> CFStringRef {
    let c = std::ffi::CString::new(value).expect("no interior NUL");
    CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_STRING_ENCODING_UTF8)
}

/// A CF dictionary from parallel key/value slices; the values are borrowed, not retained.
unsafe fn cf_dictionary(keys: &[CFStringRef], values: &[*const c_void]) -> CFDictionaryRef {
    debug_assert_eq!(keys.len(), values.len());
    CFDictionaryCreate(
        std::ptr::null(),
        keys.as_ptr(),
        values.as_ptr(),
        keys.len() as isize,
        std::ptr::null(),
        std::ptr::null(),
    )
}

/// The item reference for our service/account, without loading its data (loading would prompt).
unsafe fn item_ref() -> Option<SecKeychainItemRef> {
    let service = cf_string(KEYCHAIN_SERVICE);
    let account = cf_string(KEYCHAIN_ACCOUNT);
    let yes = core_foundation::boolean::CFBoolean::true_value();
    let keys: [CFStringRef; 4] = [
        kSecClass as CFStringRef,
        kSecAttrService as CFStringRef,
        kSecAttrAccount as CFStringRef,
        kSecReturnRef as CFStringRef,
    ];
    let values: [*const c_void; 4] = [
        kSecClassGenericPassword as *const c_void,
        service,
        account,
        yes.as_CFTypeRef(),
    ];
    let query = cf_dictionary(&keys, &values);
    let mut out: *const c_void = std::ptr::null();
    let status = SecItemCopyMatching(query, &mut out);
    CFRelease(query);
    CFRelease(service);
    CFRelease(account);
    if status != 0 || out.is_null() {
        return None;
    }
    Some(out as SecKeychainItemRef)
}

/// How the item's ACL identifies its trusted applications. Reads only ACL metadata, which needs no
/// authorization (measured: the probe reads it without a prompt).
/// What one trusted-application entry turned out to be.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AclEntry {
    /// A requirement blob this build satisfies.
    Ours,
    /// A requirement blob, but not one this build satisfies.
    Foreign,
    /// Not a requirement blob at all: a path, or a hash of an older build.
    Opaque,
}

/// Classify the collected entries. Pure, so the rule can be asserted without a keychain: the probing
/// that produces the entries needs one, and a test must not depend on this machine's item being
/// present, absent, or shaped a particular way.
fn classify_entries(entries: &[AclEntry]) -> AclIdentity {
    if entries.contains(&AclEntry::Ours) {
        AclIdentity::Requirement
    } else if entries.is_empty() {
        AclIdentity::None
    } else if entries.iter().all(|entry| *entry == AclEntry::Opaque) {
        AclIdentity::Path
    } else {
        AclIdentity::ForeignRequirement
    }
}

fn item_acl_identity() -> AclIdentity {
    unsafe {
        let Some(item) = item_ref() else {
            return AclIdentity::Unknown;
        };
        let mut access: SecAccessRef = std::ptr::null_mut();
        if SecKeychainItemCopyAccess(item, &mut access) != 0 || access.is_null() {
            CFRelease(item);
            return AclIdentity::Unknown;
        }
        let mut list: CFArrayRef = std::ptr::null();
        if SecAccessCopyACLList(access, &mut list) != 0 || list.is_null() {
            CFRelease(access);
            CFRelease(item);
            return AclIdentity::Unknown;
        }
        let mut code: SecCodeRef = std::ptr::null_mut();
        let have_code = SecCodeCopySelf(SEC_CS_DEFAULT_FLAGS, &mut code) == 0 && !code.is_null();

        let mut entries: Vec<AclEntry> = Vec::new();
        for index in 0..CFArrayGetCount(list) {
            let acl = CFArrayGetValueAtIndex(list, index);
            let mut apps: CFArrayRef = std::ptr::null();
            let mut description: CFStringRef = std::ptr::null();
            let mut prompt = CssmAclKeychainPromptSelector::default();
            let status = SecACLCopySimpleContents(
                acl as SecACLRef,
                &mut apps,
                &mut description,
                &mut prompt,
            );
            if !description.is_null() {
                CFRelease(description);
            }
            if status != 0 || apps.is_null() {
                continue;
            }
            for app_index in 0..CFArrayGetCount(apps) {
                let app = CFArrayGetValueAtIndex(apps, app_index);
                let mut data: CFDataRef = std::ptr::null();
                if SecTrustedApplicationCopyData(app as SecTrustedApplicationRef, &mut data) != 0
                    || data.is_null()
                {
                    continue;
                }
                let mut req: SecRequirementRef = std::ptr::null_mut();
                let created = SecRequirementCreateWithData(data, SEC_CS_DEFAULT_FLAGS, &mut req)
                    == 0
                    && !req.is_null();
                CFRelease(data);
                if !created {
                    // Not a requirement blob: a path, or a hash of an older build (macOS reports both
                    // this way for non-Apple-anchored apps). The *form* says nothing about stability --
                    // see this module's header, which has the measurements.
                    entries.push(AclEntry::Opaque);
                    continue;
                }
                let ours = have_code && SecCodeCheckValidity(code, SEC_CS_DEFAULT_FLAGS, req) == 0;
                entries.push(if ours {
                    AclEntry::Ours
                } else {
                    AclEntry::Foreign
                });
                CFRelease(req);
            }
            CFRelease(apps);
        }
        if have_code {
            CFRelease(code);
        }
        CFRelease(list);
        CFRelease(access);
        CFRelease(item);
        classify_entries(&entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the discriminator test below needs these two.
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> CFDataRef;
    }
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecRequirementCopyData(req: SecRequirementRef, flags: u32, data: *mut CFDataRef) -> i32;
        fn SecRequirementCreateWithString(
            text: CFStringRef,
            flags: u32,
            req: *mut SecRequirementRef,
        ) -> i32;
    }

    /// The rule the diagnostic encodes: an entry this build satisfies wins, an ACL with no entries is
    /// `none`, one that only holds path/hash entries is `path`, and requirements that are not ours read
    /// as `foreign-requirement` rather than being mistaken for a silent read.
    ///
    /// The probing that produces the entries needs a real keychain, so it is not asserted here: a test
    /// must not depend on this machine's item being present, absent, or shaped a particular way.
    #[test]
    fn the_acl_identity_follows_the_entry_forms() {
        use AclEntry::{Foreign, Opaque, Ours};
        assert_eq!(classify_entries(&[]), AclIdentity::None);
        assert_eq!(classify_entries(&[Opaque]), AclIdentity::Path);
        assert_eq!(classify_entries(&[Opaque, Opaque]), AclIdentity::Path);
        assert_eq!(
            classify_entries(&[Foreign]),
            AclIdentity::ForeignRequirement
        );
        assert_eq!(
            classify_entries(&[Ours]),
            AclIdentity::Requirement,
            "our own requirement must be recognised however else the ACL is populated"
        );
        assert_eq!(classify_entries(&[Opaque, Ours]), AclIdentity::Requirement);
        assert_eq!(classify_entries(&[Foreign, Ours]), AclIdentity::Requirement);
        // A mix is not "path-only": the foreign requirement is reported, so the reader knows the ACL
        // does hold requirement entries that simply do not admit this build.
        assert_eq!(
            classify_entries(&[Foreign, Opaque]),
            AclIdentity::ForeignRequirement
        );
    }

    /// The discriminator the diagnostic depends on: a *path* entry is not a requirement blob, so
    /// `SecRequirementCreateWithData` rejects it, while a real requirement round-trips. If this ever
    /// stopped holding, `item_acl_identity` would classify the path entry as an entry form it did not
    /// measure -- claiming a silent read, or a foreign requirement, that does not exist. Both
    /// directions are asserted, so the check cannot pass by accepting everything.
    #[test]
    fn a_path_entry_is_not_mistaken_for_a_requirement() {
        unsafe {
            let requirement = std::ffi::CString::new("identifier \"com.apple.finder\"").unwrap();
            let mut req: SecRequirementRef = std::ptr::null_mut();
            let status = SecRequirementCreateWithString(
                cf_string(&requirement.to_string_lossy()),
                SEC_CS_DEFAULT_FLAGS,
                &mut req,
            );
            assert_eq!(status, 0, "a requirement string must compile");
            assert!(!req.is_null());
            let mut blob: CFDataRef = std::ptr::null();
            assert_eq!(
                SecRequirementCopyData(req, SEC_CS_DEFAULT_FLAGS, &mut blob),
                0
            );
            assert!(!blob.is_null());
            CFRelease(req);

            // The requirement blob is accepted...
            let mut round_trip: SecRequirementRef = std::ptr::null_mut();
            assert_eq!(
                SecRequirementCreateWithData(blob, SEC_CS_DEFAULT_FLAGS, &mut round_trip),
                0,
                "a requirement blob must round-trip"
            );
            CFRelease(round_trip);

            // ...while a path (what macOS records for a newly added item) is not.
            let path_blob = CFDataCreate(
                std::ptr::null(),
                b"/Applications/Finder.app".as_ptr(),
                "/Applications/Finder.app".len() as isize,
            );
            let mut rejected: SecRequirementRef = std::ptr::null_mut();
            assert_ne!(
                SecRequirementCreateWithData(path_blob, SEC_CS_DEFAULT_FLAGS, &mut rejected),
                0,
                "a path entry must not parse as a requirement"
            );
            CFRelease(path_blob);
            CFRelease(blob);
        }
    }
}
