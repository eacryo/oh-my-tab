//! Finder context-menu subsystem.
//!
//! The main app owns the menu model and executes every action; the Finder Sync app extension
//! (finder-ext/, embedded as Oh-My-Tab-Finder.appex) only renders that model and forwards clicks.
//! The split is forced by the platform: Finder Sync extensions must be sandboxed, so the extension
//! can create no file and launch no application.
//!
//! This module builds the model from the live config, exchanges it with the extension over
//! DistributedNotificationCenter, and runs the actions a click asks for.

mod actions;
mod ipc;
mod model;
mod new_file;
mod terminals;

pub(crate) use terminals::TerminalSpec;

/// Whether the Finder extension is enabled in System Settings. Read by the settings page.
pub(crate) fn extension_enabled() -> bool {
    status::extension_enabled()
}

/// Open the system pane where the Finder extension is enabled.
pub(crate) fn open_extension_management() {
    status::open_extension_management()
}

/// Terminals this build knows about, in menu order.
pub(crate) fn terminals() -> &'static [TerminalSpec] {
    terminals::catalog()
}

/// Whether a terminal application is installed.
pub(crate) fn terminal_installed(bundle_id: &str) -> bool {
    terminals::is_installed(bundle_id)
}

/// Push the current menu model to the extension. Called after every config change that can affect
/// the menu and on startup.
pub(crate) fn broadcast_model() {
    ipc::broadcast_model();
}

/// Start the notification listener. Must run on the main thread during startup.
pub(crate) fn start() {
    ipc::start();
}

/// Tell the extension the app is quitting, so an open Finder menu stops offering items.
pub(crate) fn shutdown() {
    ipc::shutdown();
}

mod status {
    //! Reading and opening the Finder extension's system state.

    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};

    // The main app reads the class property the Finder Sync framework exposes to the containing app.
    #[link(name = "FinderSync", kind = "framework")]
    extern "C" {}

    pub(super) fn extension_enabled() -> bool {
        unsafe {
            let enabled: Bool = msg_send![class!(FIFinderSyncController), isExtensionEnabled];
            enabled.as_bool()
        }
    }

    pub(super) fn open_extension_management() {
        unsafe {
            let _: () = msg_send![
                class!(FIFinderSyncController),
                showExtensionManagementInterface
            ];
        }
    }

    #[allow(dead_code)]
    fn _assert_anyobject() {
        let _: Option<*mut AnyObject> = None;
    }
}
