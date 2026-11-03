//! Executing a click the Finder extension forwarded.
//!
//! Every request is re-validated against the live config: the menu the user clicked may have been
//! built before a settings change, and a distributed notification can be posted by any local
//! process, so the message itself is never trusted to name an enabled action.

use std::path::{Path, PathBuf};

use finder_protocol::{Context, MenuAction};

use std::sync::atomic::{AtomicBool, Ordering};

use objc2::msg_send;
use objc2::runtime::AnyObject;

use crate::config::CONFIG;
use crate::finder_menu::{hash, new_file, terminals};
use crate::i18n::t;
use crate::log_info;

/// Run one action. Called on the main thread.
pub(crate) fn execute(action: MenuAction, paths: Vec<String>, context: Context) {
    let config = CONFIG.read().unwrap().clone();
    // The paths are the user's own file names: the count is enough to diagnose, and the log
    // stays free of them.
    log_info!(
        "[finder-menu] executing {action:?} ({} target(s))",
        paths.len()
    );
    if !config.finder_menu.enabled {
        log_info!("[finder-menu] ignored a click while the menu is disabled");
        return;
    }
    match action {
        MenuAction::NewFile { extension } => {
            if !config.finder_menu.new_file_types.contains(&extension) {
                log_info!("[finder-menu] ignored a file type that is no longer enabled");
                return;
            }
            // Directory resolution and creation both touch the filesystem: a disconnected network
            // volume can block for seconds, so neither runs on the main thread.
            std::thread::spawn(move || {
                let Some(directory) = target_directory(&paths, context) else {
                    log_info!("[finder-menu] new file needs an existing directory");
                    return;
                };
                match new_file::create_in(&directory, &extension) {
                    Ok(path) => {
                        log_info!("[finder-menu] created {}", path.display());
                        reveal_on_main(path);
                    }
                    Err(error) => log_info!("[finder-menu] could not create a file: {error}"),
                }
            });
        }
        MenuAction::OpenTerminal { bundle_id } => {
            if !config.finder_menu.terminals.contains(&bundle_id) {
                log_info!("[finder-menu] ignored a terminal that is no longer enabled");
                return;
            }
            std::thread::spawn(move || {
                let Some(directory) = target_directory(&paths, context) else {
                    log_info!("[finder-menu] open terminal needs an existing directory");
                    return;
                };
                if let Err(error) = terminals::launch(&bundle_id, &directory) {
                    log_info!("[finder-menu] could not open the terminal: {error}");
                }
            });
        }
        MenuAction::CopyPath => {
            if !config.finder_menu.copy_path {
                log_info!("[finder-menu] ignored copy path while it is disabled");
                return;
            }
            let Some(path) = paths.first() else {
                return;
            };
            // The bubble says what happened, for the same reason SHA-256 has one: a menu item that
            // silently did nothing and one that copied are otherwise indistinguishable, and the
            // pasteboard can refuse the write.
            copy_path_and_report(path);
        }
        MenuAction::CopySha256 => {
            if !config.finder_menu.copy_sha256 {
                log_info!("[finder-menu] ignored copy sha256 while it is disabled");
                return;
            }
            // Re-validate everything the menu decided, because the notification can name any
            // action and the selection may have changed since the menu was built.
            if paths.len() != 1 {
                log_info!("[finder-menu] sha256 needs exactly one selection");
                return;
            }
            let path = std::path::PathBuf::from(&paths[0]);
            start_sha256(path);
        }
    }
}

/// One SHA-256 run at a time across the whole app.
///
/// Two concurrent runs would read two large files at once and could finish out of order, letting
/// the earlier click overwrite the later digest on the clipboard. A second click while one is
/// running is refused with a message rather than queued: the user can see it is busy.
static SHA256_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn start_sha256(path: PathBuf) {
    if SHA256_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        crate::ui::toast::show_done(&t("finder_menu.sha256_busy"));
        return;
    }
    // The computing message has no deadline: a large file outlives any fixed timeout, and a
    // bubble that vanished early would leave the user unable to tell it is still working.
    crate::ui::toast::show_working(&t("finder_menu.sha256_working"));
    std::thread::spawn(move || {
        let digest = hash::sha256_file(&path);
        SHA256_IN_FLIGHT.store(false, Ordering::SeqCst);
        deliver_sha256(digest);
    });
}

/// Hand the worker's result back to the main thread. AppKit and the pasteboard are main-thread
/// only, so the work is re-dispatched rather than done here.
fn deliver_sha256(result: std::io::Result<String>) {
    enum Outcome {
        Copied,
        Failed,
        Unwritable,
    }
    let outcome = match result {
        Ok(digest) => {
            if copy_text(&digest) {
                log_info!("[finder-menu] sha256 copied");
                Outcome::Copied
            } else {
                log_info!("[finder-menu] sha256 could not reach the pasteboard");
                Outcome::Unwritable
            }
        }
        Err(error) => {
            log_info!("[finder-menu] sha256 failed: {error}");
            Outcome::Failed
        }
    };
    let message = match outcome {
        Outcome::Copied => t("finder_menu.sha256_copied"),
        Outcome::Failed => t("finder_menu.sha256_failed"),
        Outcome::Unwritable => t("finder_menu.sha256_unavailable"),
    };
    // AppKit and the pasteboard are main-thread only, so the message is staged for the main
    // thread and a selector wakes it. The staged text is the only thing shared, and it is taken
    // out once, so a delayed dispatch can never show a stale result twice.
    PENDING_TOAST.lock().unwrap().replace(message);
    deliver_toast_on_main();
}

/// The message a worker finished with, waiting for the main thread to take it.
static PENDING_TOAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Wake the main thread to show whatever is staged. Safe to call from any thread: it only posts.
fn deliver_toast_on_main() {
    unsafe {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        let target = toast_target();
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: objc2::sel!(showPendingToast),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

static TOAST_TARGET: std::sync::OnceLock<crate::ffi::CallbackTarget> = std::sync::OnceLock::new();

unsafe fn toast_target() -> *mut AnyObject {
    TOAST_TARGET
        .get_or_init(|| {
            let name = c"OhMyTabFinderToastTarget";
            let class = crate::ffi::StaticClass::register_once(&TOAST_CLASS, name, || {
                crate::ffi::build_class(name, objc2::class!(NSObject), |builder| {
                    builder.add_method::<AnyObject, _>(
                        objc2::sel!(showPendingToast),
                        show_pending_toast
                            as unsafe extern "C" fn(*mut AnyObject, objc2::runtime::Sel),
                    );
                })
            });
            let instance: *mut AnyObject = msg_send![class.0 as *mut AnyObject, new];
            crate::ffi::CallbackTarget::new(instance)
        })
        .0
}

static TOAST_CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();

unsafe extern "C" fn show_pending_toast(_this: *mut AnyObject, _cmd: objc2::runtime::Sel) {
    crate::callback_guard::void("finder_show_pending_toast", || {
        // Taken, not peeked: a second dispatch for the same message would otherwise show it
        // again after the user had already seen it.
        let Some(message) = PENDING_TOAST.lock().unwrap().take() else {
            return;
        };
        crate::ui::toast::show_done(&message);
    });
}

/// Put `text` on the general pasteboard. Returns whether the write succeeded: `copy_path` ignored
/// this answer, and reporting "copied" for a failed write is worse than saying nothing.
fn copy_text(text: &str) -> bool {
    unsafe {
        use crate::ffi::{make_nsstring, release_obj};
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pasteboard.is_null() {
            return false;
        }
        let _: isize = msg_send![pasteboard, clearContents];
        let value = make_nsstring(text);
        let kind = make_nsstring("public.utf8-plain-text");
        let written: bool = msg_send![pasteboard, setString: value, forType: kind];
        release_obj(value);
        release_obj(kind);
        written
    }
}

/// The folder an action works on: the first selected item itself when it is a directory, its parent
/// when it is a file, or the clicked folder for a background/sidebar click.
fn target_directory(paths: &[String], context: Context) -> Option<PathBuf> {
    let first = paths.first()?;
    let path = PathBuf::from(first);
    match std::fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => Some(path),
        Ok(metadata) if metadata.is_file() => path.parent().map(Path::to_path_buf),
        // Toolbar menus may carry no selection at all; a container click always names a folder.
        _ => None,
    }
    .filter(|directory| directory.is_dir())
    .or_else(|| {
        (context == Context::Container)
            .then(|| paths.first().map(PathBuf::from))
            .flatten()
            .filter(|path| path.is_dir())
    })
}

/// Copy `path` and say so: the successful bubble, or the same refusal SHA-256 reports when the
/// pasteboard will not take the text.
fn copy_path_and_report(path: &str) {
    if copy_path(path) {
        log_info!("[finder-menu] path copied");
        crate::ui::toast::show_done(&t("finder_menu.path_copied"));
    } else {
        log_info!("[finder-menu] path could not reach the pasteboard");
        crate::ui::toast::show_done(&t("finder_menu.path_unavailable"));
    }
}

/// Put `path` on the general pasteboard as plain text. Returns whether the write succeeded: the
/// bubble reports the answer, so a refused write cannot read as a copy.
fn copy_path(path: &str) -> bool {
    unsafe {
        use crate::ffi::{make_nsstring, release_obj};
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pasteboard.is_null() {
            return false;
        }
        let _: isize = msg_send![pasteboard, clearContents];
        let value = make_nsstring(path);
        let kind = make_nsstring("public.utf8-plain-text");
        let written: bool = msg_send![pasteboard, setString: value, forType: kind];
        release_obj(value);
        release_obj(kind);
        written
    }
}

/// Select the new file in Finder. AppKit is main-thread only and the file was created on a worker,
/// so the path is handed to a small ObjC helper and re-dispatched with performSelectorOnMainThread.
fn reveal_on_main(path: PathBuf) {
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{class, msg_send, sel};

    use crate::ffi::{build_class, make_nsstring, release_obj, StaticClass};
    use std::sync::OnceLock;

    static HELPER: OnceLock<usize> = OnceLock::new();

    unsafe extern "C" fn reveal_path(_this: *mut AnyObject, _cmd: Sel, path: *mut AnyObject) {
        crate::callback_guard::void("finder_reveal_path", || {
            let text = unsafe { crate::ffi::nsstring_to_rust(path) };
            unsafe { reveal(Path::new(&text)) };
        });
    }

    let helper = *HELPER.get_or_init(|| unsafe {
        static CLASS: OnceLock<StaticClass> = OnceLock::new();
        let class = StaticClass::register_once(&CLASS, c"OhMyTabFinderReveal", || {
            build_class(c"OhMyTabFinderReveal", class!(NSObject), |builder| {
                builder.add_method::<AnyObject, _>(
                    sel!(revealPath:),
                    reveal_path as unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
                );
            })
        });
        let object: *mut AnyObject = msg_send![class.0 as *mut AnyObject, new];
        object as usize
    }) as *mut AnyObject;
    if helper.is_null() {
        return;
    }
    unsafe {
        let text = make_nsstring(&path.to_string_lossy());
        let _: () = msg_send![
            helper,
            performSelectorOnMainThread: sel!(revealPath:),
            withObject: text,
            waitUntilDone: false
        ];
        release_obj(text);
    }
}

unsafe fn reveal(path: &Path) {
    use crate::ffi::{make_nsstring, release_obj};
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    let text = make_nsstring(&path.to_string_lossy());
    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: text];
    release_obj(text);
    if url.is_null() {
        return;
    }
    let array: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: url];
    if array.is_null() {
        return;
    }
    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
    let _: () = msg_send![workspace, activateFileViewerSelecting: array];
}
