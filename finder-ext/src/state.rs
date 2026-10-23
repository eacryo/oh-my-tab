//! Extension-side state and the distributed-notification link to the main app.
//!
//! The extension never reads the config: it caches the last menu model the app pushed and asks for
//! a fresh one when it starts or when the cache has gone stale. A click is reported to the app,
//! which re-validates it against the live config before doing anything.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use finder_protocol::{
    decode, encode, AppToExt, ClickTemplate, Context, ExtToApp, MenuModel, Trigger,
    PROTOCOL_VERSION,
};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};

use crate::ffi::{log, nsstring, nsstring_to_rust};

/// How long the cached model is trusted without a message from the app. The extension syncs every
/// few seconds, so this only expires when the app died.
const MODEL_TTL: Duration = Duration::from_secs(45);
const SYNC_INTERVAL: Duration = Duration::from_secs(10);

struct Shared {
    host_bundle_id: String,
    /// The app instance whose model is cached. A click carries it so the app can reject a menu
    /// built by an earlier process; a sync carries it so the app knows to resend after a restart.
    host_instance: Mutex<String>,
    model: Mutex<Option<MenuModel>>,
    last_state: Mutex<Instant>,
    model_version: AtomicU64,
    request_counter: AtomicU64,
}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// The host bundle id is written into the appex Info.plist when the bundle is assembled, so the
/// dev and release channels never share a notification channel.
pub(crate) fn install() {
    let Some(host_bundle_id) = host_bundle_id() else {
        log("no OMTHostBundleIdentifier in Info.plist; extension disabled");
        return;
    };
    let shared = SHARED.get_or_init(|| Shared {
        host_bundle_id: host_bundle_id.clone(),
        host_instance: Mutex::new(String::new()),
        model: Mutex::new(None),
        last_state: Mutex::new(Instant::now()),
        model_version: AtomicU64::new(0),
        request_counter: AtomicU64::new(0),
    });
    let _ = shared;
    unsafe {
        register_observer(&host_bundle_id);
    }
    // The app may already be running: ask for its model, and keep asking so a restarted app is
    // noticed without a Finder restart.
    std::thread::spawn(|| loop {
        std::thread::sleep(SYNC_INTERVAL);
        // Foundation objects created on this thread (the notification name and payload strings)
        // are autoreleased; without a pool of its own this thread never drains them.
        unsafe {
            let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
            send_sync();
            let _: () = msg_send![pool, drain];
        }
    });
    send_sync();
}

fn host_bundle_id() -> Option<String> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        if bundle.is_null() {
            return None;
        }
        let key = nsstring("OMTHostBundleIdentifier");
        let value: *mut AnyObject = msg_send![bundle, objectForInfoDictionaryKey: key];
        if value.is_null() {
            return None;
        }
        let bundle_id = nsstring_to_rust(value);
        (!bundle_id.is_empty()).then_some(bundle_id)
    }
}

fn shared() -> Option<&'static Shared> {
    SHARED.get()
}

fn send_sync() {
    let Some(shared) = shared() else {
        return;
    };
    let message = ExtToApp::Sync {
        protocol_version: PROTOCOL_VERSION,
        host_bundle_id: shared.host_bundle_id.clone(),
        host_instance: shared
            .host_instance
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone(),
        model_version: shared.model_version.load(Ordering::SeqCst),
    };
    post(&shared.host_bundle_id, &message);
}

/// The model to render, or None when the app is gone.
pub(crate) fn model(trigger: Context) -> Option<MenuModel> {
    let shared = shared()?;
    let fresh = *shared
        .last_state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if model_is_stale(fresh.elapsed(), MODEL_TTL) {
        return None;
    }
    let model = shared
        .model
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()?;
    let filtered = finder_protocol::filter_model(&model, trigger);
    (!filtered.items.is_empty()).then_some(filtered)
}

/// Report a click. The app resolves the selection paths; a spoofed message cannot name an action
/// the config does not enable.
pub(crate) fn send_click(template: ClickTemplate) {
    let Some(shared) = shared() else {
        return;
    };
    let paths = unsafe { selection_paths(template.trigger) };
    let request = format!(
        "{}-{}",
        std::process::id(),
        shared.request_counter.fetch_add(1, Ordering::SeqCst)
    );
    let message = ExtToApp::Click {
        protocol_version: PROTOCOL_VERSION,
        host_bundle_id: shared.host_bundle_id.clone(),
        // The template's own identity, not the cache's: a menu built by an earlier app process
        // must be reported as such so the app can reject it.
        host_instance: template.host_instance,
        model_version: template.model_version,
        request_id: request,
        action: template.action,
        paths,
        trigger: template.trigger,
    };
    post(&shared.host_bundle_id, &message);
}

unsafe fn selection_paths(trigger: Trigger) -> Vec<String> {
    let controller: *mut AnyObject = msg_send![class!(FIFinderSyncController), defaultController];
    if controller.is_null() {
        return Vec::new();
    }
    let mut urls: *mut AnyObject = std::ptr::null_mut();
    if trigger == Trigger::Items {
        urls = msg_send![controller, selectedItemURLs];
    }
    if urls.is_null() {
        urls = msg_send![controller, targetedURL];
    }
    if urls.is_null() {
        return Vec::new();
    }
    // selectedItemURLs is an NSArray; targetedURL is a single NSURL.
    let is_array: bool = msg_send![urls, isKindOfClass: class!(NSArray)];
    let mut paths = Vec::new();
    if is_array {
        let count: usize = msg_send![urls, count];
        for index in 0..count {
            let url: *mut AnyObject = msg_send![urls, objectAtIndex: index];
            push_url_path(url, &mut paths);
        }
    } else {
        push_url_path(urls, &mut paths);
    }
    paths
}

unsafe fn push_url_path(url: *mut AnyObject, paths: &mut Vec<String>) {
    if url.is_null() {
        return;
    }
    let path: *mut AnyObject = msg_send![url, path];
    let path = nsstring_to_rust(path);
    if !path.is_empty() {
        paths.push(path);
    }
}

fn post(bundle_id: &str, message: &ExtToApp) {
    let Some(json) = encode(message) else {
        log("could not encode an extension message");
        return;
    };
    unsafe {
        let center: *mut AnyObject =
            msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        if center.is_null() {
            return;
        }
        let name = nsstring(&format!("{bundle_id}.finder.ext2app"));
        let payload = nsstring(&json);
        let _: () = msg_send![
            center,
            postNotificationName: name,
            object: payload,
            userInfo: std::ptr::null::<AnyObject>(),
            deliverImmediately: true
        ];
    }
}

unsafe fn register_observer(bundle_id: &str) {
    let class = observer_class();
    let observer: *mut AnyObject = msg_send![class, new];
    if observer.is_null() {
        return;
    }
    let center: *mut AnyObject = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
    let name = nsstring(&format!("{bundle_id}.finder.app2ext"));
    let _: () = msg_send![
        center,
        addObserver: observer,
        selector: sel!(handleAppMessage:),
        name: name,
        object: std::ptr::null::<AnyObject>()
    ];
}

fn observer_class() -> *mut AnyObject {
    use objc2::runtime::ClassBuilder;
    /// The process-lifetime class pointer. Registration happens once; the raw pointer is handed to
    /// the runtime, which owns the class for the life of the process.
    struct ClassPtr(*mut AnyObject);
    unsafe impl Send for ClassPtr {}
    unsafe impl Sync for ClassPtr {}
    static CLASS: OnceLock<ClassPtr> = OnceLock::new();
    CLASS
        .get_or_init(|| unsafe {
            let mut builder =
                ClassBuilder::new(c"OhMyTabFinderExtObserver", class!(NSObject)).expect("observer");
            builder.add_method::<AnyObject, _>(
                sel!(handleAppMessage:),
                handle_app_message as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            ClassPtr(builder.register() as *const _ as *mut AnyObject)
        })
        .0
}

unsafe extern "C" fn handle_app_message(
    _this: *mut AnyObject,
    _cmd: Sel,
    notification: *mut AnyObject,
) {
    crate::guard::void("handleAppMessage", || unsafe {
        if notification.is_null() {
            return;
        }
        let payload: *mut AnyObject = msg_send![notification, object];
        if payload.is_null() {
            return;
        }
        handle_message(&nsstring_to_rust(payload));
    });
}

fn handle_message(json: &str) {
    let Some(shared) = shared() else {
        return;
    };
    let Some(message): Option<AppToExt> = decode(json) else {
        log(&format!(
            "dropped a malformed app message ({} bytes)",
            json.len()
        ));
        return;
    };
    match message {
        AppToExt::State {
            protocol_version,
            host_bundle_id,
            host_instance,
            model,
        } => {
            if protocol_version != PROTOCOL_VERSION || host_bundle_id != shared.host_bundle_id {
                return;
            }
            let update = cache_update(
                &shared
                    .host_instance
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone(),
                &host_instance,
                model.as_ref().map(|model| model.model_version),
                shared.model_version.load(Ordering::SeqCst),
            );
            *shared
                .host_instance
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = host_instance;
            if update.invalidate || update.adopt_version.is_some() {
                let mut cached = shared
                    .model
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if update.invalidate {
                    *cached = None;
                }
                if let Some(incoming) = model.as_ref() {
                    if update.adopt_version.is_some() {
                        *cached = Some(incoming.clone());
                    }
                }
            }
            if let Some(version) = update.adopt_version {
                shared.model_version.store(version, Ordering::SeqCst);
            }
            *shared
                .last_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
        }
        AppToExt::Ack { .. } => {
            *shared
                .last_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
        }
        AppToExt::Quit => {
            *shared
                .model
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            *shared
                .last_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now() - MODEL_TTL;
        }
    }
}
/// Whether the cached model has outlived the app's last message. Pure, so the rule can be asserted
/// without waiting: a model older than the TTL means the app is gone and no items are offered.
fn model_is_stale(elapsed: std::time::Duration, ttl: std::time::Duration) -> bool {
    elapsed > ttl
}

#[cfg(test)]
mod tests {
    use super::model_is_stale;
    use std::time::Duration;

    #[test]
    fn a_model_older_than_the_ttl_is_stale() {
        assert!(model_is_stale(
            Duration::from_secs(46),
            Duration::from_secs(45)
        ));
    }

    #[test]
    fn a_freshly_synced_model_is_not_stale() {
        assert!(!model_is_stale(
            Duration::from_secs(5),
            Duration::from_secs(45)
        ));
        assert!(!model_is_stale(
            Duration::from_secs(45),
            Duration::from_secs(45)
        ));
    }
}
/// What a received `State` means for the extension's cache.
#[derive(Debug, PartialEq, Eq)]
struct CacheUpdate {
    /// The cached model belongs to another app process and must be dropped.
    invalidate: bool,
    /// The received model is authoritative; adopt it and this version.
    adopt_version: Option<u64>,
}

/// Decide what to do with a State message.
///
/// Pure, because the failure mode is a state transition, not a rendering one: an app restart
/// resets its model versions, so an instance change always invalidates what the cache holds. When
/// the message carries no model (the app had nothing newer to send), the old model must NOT be
/// re-labelled as belonging to the new instance -- that would leave a stale menu looking current and
/// suppress every later resend.
fn cache_update(
    cached_instance: &str,
    incoming_instance: &str,
    incoming_version: Option<u64>,
    cached_version: u64,
) -> CacheUpdate {
    let instance_changed = cached_instance != incoming_instance;
    match (instance_changed, incoming_version) {
        (true, None) => CacheUpdate {
            invalidate: true,
            adopt_version: Some(0),
        },
        (true, Some(version)) => CacheUpdate {
            invalidate: false,
            adopt_version: Some(version),
        },
        (false, Some(version)) if version >= cached_version => CacheUpdate {
            invalidate: false,
            adopt_version: Some(version),
        },
        _ => CacheUpdate {
            invalidate: false,
            adopt_version: None,
        },
    }
}

#[cfg(test)]
mod cache_tests {
    use super::cache_update;

    /// The sequence from the review: extension A misses the new instance's first broadcast, then
    /// receives a model-less State from a sibling's sync. The old model must be dropped and the
    /// version reset, so A's next sync asks for a full model again.
    #[test]
    fn a_model_less_state_from_a_new_instance_invalidates_the_cache() {
        let update = cache_update("old", "new", None, 7);
        assert!(update.invalidate);
        assert_eq!(update.adopt_version, Some(0));
    }

    #[test]
    fn a_new_instance_with_a_model_is_adopted_even_if_its_version_is_lower() {
        let update = cache_update("old", "new", Some(2), 7);
        assert!(!update.invalidate);
        assert_eq!(update.adopt_version, Some(2));
    }

    #[test]
    fn the_same_instance_keeps_a_newer_cached_model() {
        let update = cache_update("same", "same", Some(1), 3);
        assert!(!update.invalidate);
        assert_eq!(update.adopt_version, None);
    }

    #[test]
    fn the_same_instance_adopts_an_equal_or_newer_version() {
        assert_eq!(
            cache_update("same", "same", Some(3), 3).adopt_version,
            Some(3)
        );
        assert_eq!(
            cache_update("same", "same", Some(4), 3).adopt_version,
            Some(4)
        );
    }
}
