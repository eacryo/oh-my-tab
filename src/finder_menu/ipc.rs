//! The distributed-notification bridge between the main app and the Finder extension.
//!
//! The extension is sandboxed, so it cannot read the config or write a file; it only renders the
//! menu model this module pushes and forwards clicks back. Each channel is derived from the host
//! bundle id (dev and release never talk to each other), and every message carries the app's launch
//! instance so a menu built by an earlier app process cannot execute against the current one.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};

use crate::ffi::{build_class, make_nsstring, nsstring_to_rust, release_obj, StaticClass};
use crate::i18n::{t, tf};
use crate::log_info;
use finder_protocol::{
    decode, encode, AppToExt, ClickRefusal, ExtToApp, MenuAction, PromisedSelection,
};

/// Identity of this app process and the menu model it is publishing.
struct Host {
    bundle_id: String,
    instance: String,
    model_version: AtomicU64,
}

static HOST: OnceLock<Host> = OnceLock::new();
/// The notification observer object, kept for the process lifetime (it owns no state of its own).
static OBSERVER: AtomicUsize = AtomicUsize::new(0);

fn host() -> Option<&'static Host> {
    HOST.get()
}

fn channel(bundle_id: &str, direction: &str) -> String {
    format!("{bundle_id}.finder.{direction}")
}

/// Start listening and publish the first model. Runs on the main thread during startup.
pub(crate) fn start() {
    let Some(bundle_id) = main_bundle_id() else {
        log_info!("[finder-menu] no bundle identifier (unbundled run); extension bridge disabled");
        return;
    };
    let instance = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    );
    if HOST
        .set(Host {
            bundle_id: bundle_id.clone(),
            instance,
            model_version: AtomicU64::new(0),
        })
        .is_err()
    {
        return;
    }
    unsafe {
        install_observer(&bundle_id);
    }
    broadcast_model();
    log_info!("[finder-menu] bridge started for {bundle_id}");
}

/// Tell the extension the app is going away, so an open Finder menu stops offering items.
pub(crate) fn shutdown() {
    if let Some(host) = host() {
        post(&host.bundle_id, &AppToExt::Quit);
    }
}

/// Publish the current model (or an empty one when the feature is off).
///
/// A broadcast is addressed to the current protocol. An extension process left over from before an
/// update speaks v2 and drops it; its own next `Sync` is answered with the v2 model instead, so its
/// menu keeps working until Finder replaces it.
pub(crate) fn broadcast_model() {
    let Some(host) = host() else {
        return;
    };
    let version = host.model_version.fetch_add(1, Ordering::SeqCst) + 1;
    let config = crate::config::CONFIG.read().unwrap().clone();
    let model = crate::finder_menu::model::build(
        &config,
        &host.bundle_id,
        &host.instance,
        version,
        finder_protocol::PROTOCOL_VERSION,
    );
    post(
        &host.bundle_id,
        &AppToExt::State {
            protocol_version: finder_protocol::PROTOCOL_VERSION,
            host_bundle_id: host.bundle_id.clone(),
            host_instance: host.instance.clone(),
            model: Some(model),
        },
    );
}

fn main_bundle_id() -> Option<String> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        if bundle.is_null() {
            return None;
        }
        let identifier: *mut AnyObject = msg_send![bundle, bundleIdentifier];
        if identifier.is_null() {
            return None;
        }
        Some(nsstring_to_rust(identifier))
    }
}

fn post(bundle_id: &str, message: &AppToExt) {
    let Some(json) = encode(message) else {
        log_info!("[finder-menu] could not encode an app message; dropped");
        return;
    };
    unsafe {
        let center: *mut AnyObject =
            msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
        if center.is_null() {
            return;
        }
        let name = make_nsstring(&channel(bundle_id, "app2ext"));
        let payload = make_nsstring(&json);
        let _: () = msg_send![
            center,
            postNotificationName: name,
            object: payload,
            userInfo: std::ptr::null::<AnyObject>(),
            deliverImmediately: true
        ];
        release_obj(name);
        release_obj(payload);
    }
}

unsafe fn install_observer(bundle_id: &str) {
    let class = observer_class();
    let observer: *mut AnyObject = msg_send![class, new];
    if observer.is_null() {
        return;
    }
    OBSERVER.store(observer as usize, Ordering::SeqCst);

    // Extension -> app channel.
    let center: *mut AnyObject = msg_send![class!(NSDistributedNotificationCenter), defaultCenter];
    let name = make_nsstring(&channel(bundle_id, "ext2app"));
    let _: () = msg_send![
        center,
        addObserver: observer,
        selector: sel!(handleExtMessage:),
        name: name,
        object: std::ptr::null::<AnyObject>()
    ];
    release_obj(name);

    // Orderly quit: the extension clears its model so no dead items stay in Finder's menu.
    let notifications: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let terminate = make_nsstring("NSApplicationWillTerminateNotification");
    let _: () = msg_send![
        notifications,
        addObserver: observer,
        selector: sel!(handleWillTerminate:),
        name: terminate,
        object: std::ptr::null::<AnyObject>()
    ];
    release_obj(terminate);
}

fn observer_class() -> *mut AnyObject {
    static CLASS: OnceLock<StaticClass> = OnceLock::new();
    StaticClass::register_once(&CLASS, c"OhMyTabFinderMenuIpc", || {
        build_class(
            c"OhMyTabFinderMenuIpc",
            class!(NSObject),
            |builder| unsafe {
                builder.add_method::<AnyObject, _>(
                    sel!(handleExtMessage:),
                    handle_ext_message as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(handleOnMain:),
                    handle_on_main as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
                builder.add_method::<AnyObject, _>(
                    sel!(handleWillTerminate:),
                    handle_will_terminate
                        as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            },
        )
    })
    .0 as *mut AnyObject
}

/// Distributed notifications arrive on a notification thread; everything that touches the config or
/// AppKit is re-dispatched to the main thread.
unsafe extern "C" fn handle_ext_message(
    _this: *mut AnyObject,
    _cmd: Sel,
    notification: *mut AnyObject,
) {
    crate::callback_guard::void("finder_ext_message", || unsafe {
        if notification.is_null() {
            return;
        }
        let payload: *mut AnyObject = msg_send![notification, object];
        if payload.is_null() {
            return;
        }
        let json = nsstring_to_rust(payload);
        let boxed = make_nsstring(&json);
        let observer = OBSERVER.load(Ordering::SeqCst) as *mut AnyObject;
        if observer.is_null() {
            release_obj(boxed);
            return;
        }
        let _: () = msg_send![
            observer,
            performSelectorOnMainThread: sel!(handleOnMain:),
            withObject: boxed,
            waitUntilDone: false
        ];
        release_obj(boxed);
    });
}

unsafe extern "C" fn handle_on_main(_this: *mut AnyObject, _cmd: Sel, payload: *mut AnyObject) {
    crate::callback_guard::void("finder_ext_message_main", || unsafe {
        if payload.is_null() {
            return;
        }
        handle_message(&nsstring_to_rust(payload));
    });
}

unsafe extern "C" fn handle_will_terminate(
    _this: *mut AnyObject,
    _cmd: Sel,
    _notification: *mut AnyObject,
) {
    crate::callback_guard::void("finder_will_terminate", shutdown);
}

fn handle_message(json: &str) {
    let Some(host) = host() else {
        return;
    };
    let Some(message): Option<ExtToApp> = decode(json) else {
        log_info!("[finder-menu] dropped a malformed extension message");
        return;
    };
    match message {
        ExtToApp::Sync {
            protocol_version,
            host_bundle_id,
            host_instance,
            model_version,
        } => {
            // A v2 extension is the process an update left behind; it is answered in its own
            // shapes so its menu keeps working until Finder replaces it. Anything else is a
            // stranger and is dropped exactly as before.
            let Some(peer_protocol) = finder_protocol::app_peer_protocol(protocol_version) else {
                return;
            };
            if host_bundle_id != host.bundle_id {
                return;
            }
            let current = host.model_version.load(Ordering::SeqCst);
            let model = sync_needs_model(&host.instance, &host_instance, model_version, current)
                .then(|| {
                    let config = crate::config::CONFIG.read().unwrap().clone();
                    crate::finder_menu::model::build(
                        &config,
                        &host.bundle_id,
                        &host.instance,
                        current,
                        peer_protocol,
                    )
                });
            post(
                &host.bundle_id,
                &AppToExt::State {
                    protocol_version: peer_protocol,
                    host_bundle_id: host.bundle_id.clone(),
                    host_instance: host.instance.clone(),
                    model,
                },
            );
        }
        ExtToApp::Click {
            protocol_version,
            host_bundle_id,
            host_instance,
            request_id,
            action,
            paths,
            trigger,
            ..
        } => {
            log_info!(
                "[finder-menu] click received: action={action:?} trigger={trigger:?} paths={} instance_match={}",
                paths.len(),
                host_instance == host.instance
            );
            // A v2 extension can only send the two single-selection actions; the shape check below
            // is what stops a v3 action from being acted on with a v2 banner.
            if finder_protocol::app_peer_protocol(protocol_version).is_none() {
                log_info!(
                    "[finder-menu] dropped a click: protocol {protocol_version} vs {}",
                    finder_protocol::PROTOCOL_VERSION
                );
                return;
            }
            if host_bundle_id != host.bundle_id {
                log_info!("[finder-menu] dropped a click from another host channel");
                return;
            }
            if host_instance != host.instance {
                // A menu built by an earlier app process asked for this; its model may not
                // describe the current config, so the click is dropped.
                log_info!("[finder-menu] dropped a click from a previous app instance");
                return;
            }
            // The item promised a shape. A menu can outlive a selection change, and a distributed
            // notification can name any action, so a click that no longer matches its item is
            // refused instead of doing something the label did not promise.
            if !action_enabled_now(&action) {
                log_info!("[finder-menu] dropped a click for an action that is now disabled");
                return;
            }
            if !click_matches_promise(&action, paths.len(), protocol_version) {
                log_info!(
                    "[finder-menu] dropped a click that no longer matches its item ({} path(s), promised {:?})",
                    paths.len(),
                    action.promised_selection()
                );
                crate::ui::toast::show_refused(&t("finder_menu.selection_changed"));
                return;
            }
            post(
                &host.bundle_id,
                &AppToExt::Ack {
                    request_id: request_id.clone(),
                },
            );
            crate::finder_menu::actions::execute(action, paths, trigger.context());
        }
        ExtToApp::ClickRefused {
            protocol_version,
            host_bundle_id,
            host_instance,
            action,
            count,
            reason,
        } => {
            if protocol_version != finder_protocol::PROTOCOL_VERSION
                || host_bundle_id != host.bundle_id
                || host_instance != host.instance
            {
                log_info!("[finder-menu] dropped a refusal from another instance or protocol");
                return;
            }
            log_info!("[finder-menu] click refused: {reason:?} for {action:?} ({count} item(s))");
            // A refusal is a status message about an action the user can no longer take; with the
            // feature or that action switched off it must be as quiet as a click would be.
            if !action_enabled_now(&action) {
                log_info!("[finder-menu] dropped a refusal for an action that is now disabled");
                return;
            }
            // The click never arrived, so this is the only feedback the user gets. It is worth a
            // message: a menu that silently did nothing is indistinguishable from a broken one.
            let message = match reason {
                ClickRefusal::TooLarge => tf(
                    "finder_menu.selection_too_large",
                    &[("count", &count.to_string())],
                ),
                ClickRefusal::IncompletePaths => t("finder_menu.selection_incomplete"),
            };
            crate::ui::toast::show_refused(&message);
        }
    }
}
/// Whether a sync has to be answered with the full model.
///
/// Two independent staleness signals: a different app instance (its versions restart from 1, so a
/// higher cached version from the previous process means nothing) and an older model version within
/// the same instance. Pure so both cases are asserted without a live notification.
fn sync_needs_model(
    our_instance: &str,
    their_instance: &str,
    their_version: u64,
    our_version: u64,
) -> bool {
    our_instance != their_instance || their_version < our_version
}

/// Whether the live config still enables the action a message names, so a stale menu cannot make
/// the app speak about something the user has turned off.
fn action_enabled_now(action: &MenuAction) -> bool {
    let config = crate::config::CONFIG.read().unwrap();
    crate::finder_menu::actions::action_enabled(&config, action)
}

/// Whether a click still has the shape its menu item promised.
///
/// The action, not the item's target, is what the app has: a v2 click carries no target at all, and
/// a spoofed click can name any action. `Any` actions are unconstrained. Pure, so the rule is
/// asserted without a notification.
fn click_matches_promise(action: &MenuAction, paths: usize, protocol: u32) -> bool {
    // A v2 menu's "Copy Path" item was built for any selection and always copied the first path.
    // Within the compatibility window that item is still in the menu for several selected items, so
    // applying v3's "exactly one" rule would take a working action away from the user.
    if protocol < 3 && matches!(action, MenuAction::CopyPath) {
        return paths >= 1;
    }
    match action.promised_selection() {
        PromisedSelection::Any => true,
        PromisedSelection::One => paths == 1,
        PromisedSelection::Many => paths >= 2,
    }
}

#[cfg(test)]
mod tests {
    use super::{click_matches_promise, sync_needs_model};
    use finder_protocol::{MenuAction, PathLayout};

    #[test]
    fn a_restarted_app_resends_even_to_a_higher_cached_version() {
        // The extension cached version 7 from the previous app process; the new process is at 2.
        assert!(sync_needs_model("new", "old", 7, 2));
    }

    #[test]
    fn a_current_extension_needs_no_resend() {
        assert!(!sync_needs_model("same", "same", 3, 3));
        assert!(!sync_needs_model("same", "same", 4, 3));
    }

    #[test]
    fn an_older_version_within_the_same_instance_is_resent() {
        assert!(sync_needs_model("same", "same", 1, 3));
    }

    /// The single item must never act on several paths, and the multiple items must never act on
    /// one: a menu that outlived a selection change would otherwise do something its label did not
    /// say. The unconstrained actions keep working on any count.
    #[test]
    fn a_click_is_accepted_only_with_the_shape_its_item_promised() {
        let single = [MenuAction::CopyPath, MenuAction::CopySha256];
        for action in &single {
            for paths in [0, 1, 2, 50] {
                assert_eq!(
                    click_matches_promise(action, paths, 3),
                    paths == 1,
                    "{action:?} with {paths} path(s)"
                );
            }
        }
        let many = [
            MenuAction::CopyPaths {
                layout: PathLayout::Newline,
            },
            MenuAction::CopyPaths {
                layout: PathLayout::SingleLine,
            },
            MenuAction::CopySha256Many { with_name: false },
            MenuAction::CopySha256Many { with_name: true },
        ];
        for action in &many {
            for paths in [0, 1, 2, 50] {
                assert_eq!(
                    click_matches_promise(action, paths, 3),
                    paths >= 2,
                    "{action:?} with {paths} path(s)"
                );
            }
        }
        let any = [
            MenuAction::NewFile {
                extension: "md".into(),
            },
            MenuAction::OpenTerminal {
                bundle_id: "com.apple.Terminal".into(),
            },
        ];
        for action in &any {
            for paths in [0, 1, 2, 50] {
                assert!(
                    click_matches_promise(action, paths, 3),
                    "{action:?} {paths}"
                );
            }
        }
    }

    /// The compatibility window must keep the v2 action working: its menu shows "Copy Path" for a
    /// multi-selection too, and v2 copied the first path. v3's stricter rule would make that click
    /// do nothing.
    #[test]
    fn a_v2_copy_path_keeps_its_first_path_behavior() {
        assert!(click_matches_promise(&MenuAction::CopyPath, 2, 2));
        assert!(click_matches_promise(&MenuAction::CopyPath, 50, 2));
        assert!(!click_matches_promise(&MenuAction::CopyPath, 0, 2));
        assert!(!click_matches_promise(&MenuAction::CopyPath, 2, 3));
        // v2's digest item was already single-only, and stays that way.
        assert!(!click_matches_promise(&MenuAction::CopySha256, 2, 2));
        assert!(click_matches_promise(&MenuAction::CopySha256, 1, 2));
    }
}
