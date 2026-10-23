//! The Finder Sync app extension.
//!
//! The process entry point is Foundation's NSExtensionMain (see build.rs): the extension host
//! starts the executable, NSExtension reads the appex Info.plist and instantiates the principal
//! class. The class is registered from a Rust main before control is handed over, because the
//! extension host resolves NSExtensionPrincipalClass by name once the process is up.
//!
//! The extension is thin by design: it renders the menu model the main app pushes and forwards
//! clicks. It cannot touch files or launch applications — Finder Sync extensions are sandboxed.

mod ffi;
mod guard;
mod menu;
mod state;

use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
use objc2::{class, msg_send, sel};
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::OnceLock;

use ffi::{log, nsstring};
use finder_protocol::Trigger;

// Each framework gets its own link attribute: one extern block can carry only one #[link].
#[link(name = "Foundation", kind = "framework")]
extern "C" {}
#[link(name = "AppKit", kind = "framework")]
extern "C" {}
#[link(name = "FinderSync", kind = "framework")]
extern "C" {}

/// The principal class name; assets/FinderExtension/Info.plist states the same string.
const PRINCIPAL_CLASS: &CStr = c"OhMyTabFinderSync";

fn main() {
    register_principal_class();
    state::install();
    let args: Vec<CString> = std::env::args_os()
        .map(|arg| {
            use std::os::unix::ffi::OsStringExt;
            CString::new(arg.into_vec()).unwrap_or_default()
        })
        .collect();
    let mut argv: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).collect();
    argv.push(std::ptr::null());
    unsafe {
        extern "C" {
            fn NSExtensionMain(argc: i32, argv: *const *const c_char) -> i32;
        }
        let status = NSExtensionMain(argv.len() as i32 - 1, argv.as_ptr());
        std::process::exit(status);
    }
}

fn register_principal_class() {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| unsafe {
        let mut builder =
            ClassBuilder::new(PRINCIPAL_CLASS, class!(FIFinderSync)).expect("principal class");
        builder.add_method::<AnyObject, _>(
            sel!(init),
            init as unsafe extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject,
        );
        builder.add_method::<AnyObject, _>(
            sel!(menuForMenuKind:),
            menu_for_menu_kind
                as unsafe extern "C" fn(*mut AnyObject, Sel, usize) -> *mut AnyObject,
        );
        builder.add_method::<AnyObject, _>(
            sel!(handleMenuClick:),
            handle_menu_click as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
        );
        let class = builder.register();
        log(&format!("registered principal class at {class:p}"));
        class
    });
}

unsafe extern "C" fn init(this: *mut AnyObject, _cmd: Sel) -> *mut AnyObject {
    let superclass = objc2::ffi::class_getSuperclass(msg_send![this, class]);
    #[repr(C)]
    struct ObjcSuper {
        receiver: *mut c_void,
        super_class: *mut c_void,
    }
    extern "C" {
        fn objc_msgSendSuper();
    }
    let mut sup = ObjcSuper {
        receiver: this as *mut c_void,
        super_class: superclass as *mut c_void,
    };
    let send: unsafe extern "C" fn(*mut ObjcSuper, Sel) -> *mut AnyObject =
        std::mem::transmute(objc_msgSendSuper as *const ());
    let this = send(&mut sup, sel!(init));
    if !this.is_null() {
        // Finder only offers the menu inside the directories the extension declares, so this has
        // to run once the extension context exists: before NSExtensionMain the controller is nil
        // and setting directoryURLs silently does nothing.
        install_directory_urls();
    }
    this
}

/// Declare the directories Finder should offer the menu in: "/" covers the boot volume and the
/// mounted volumes are added as they appear, because a sandboxed extension cannot enumerate a
/// volume's contents but can declare the volume root.
unsafe fn install_directory_urls() {
    let controller: *mut AnyObject = msg_send![class!(FIFinderSyncController), defaultController];
    if controller.is_null() {
        log("FIFinderSyncController unavailable while registering");
        return;
    }
    let root = nsstring("/");
    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: root];
    let set: *mut AnyObject = msg_send![class!(NSSet), setWithObject: url];
    let _: () = msg_send![controller, setDirectoryURLs: set];
}

unsafe extern "C" fn menu_for_menu_kind(
    this: *mut AnyObject,
    _cmd: Sel,
    kind: usize,
) -> *mut AnyObject {
    guard::query("menuForMenuKind", std::ptr::null_mut::<AnyObject>(), || {
        let trigger = trigger_for_kind(kind);
        log(&format!("menu requested kind={} trigger={trigger:?}", kind));
        menu::build(trigger, this).unwrap_or(std::ptr::null_mut())
    })
}

fn trigger_for_kind(kind: usize) -> Trigger {
    // FIMenuKind: 0 items, 1 container, 2 sidebar, 3 toolbar.
    match kind {
        1 => Trigger::Container,
        2 => Trigger::Sidebar,
        3 => Trigger::Toolbar,
        _ => Trigger::Items,
    }
}

unsafe extern "C" fn handle_menu_click(_this: *mut AnyObject, _cmd: Sel, sender: *mut AnyObject) {
    guard::void("handleMenuClick", || unsafe {
        // A literal format string is not redacted in the unified log, unlike a formatted one, so
        // these two markers are readable without any entitlements.
        ffi::log_static("[finder-ext] menu item clicked");
        let Some(template) = menu::template_for(sender) else {
            ffi::log_static("[finder-ext] menu item had no click template");
            return;
        };
        state::send_click(template);
        ffi::log_static("[finder-ext] click posted");
    });
}
