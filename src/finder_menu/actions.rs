//! Executing a click the Finder extension forwarded.
//!
//! Every request is re-validated against the live config: the menu the user clicked may have been
//! built before a settings change, and a distributed notification can be posted by any local
//! process, so the message itself is never trusted to name an enabled action.

use std::path::{Path, PathBuf};

use finder_protocol::{Context, MenuAction};

use crate::config::CONFIG;
use crate::finder_menu::{new_file, terminals};
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
            copy_path(path);
        }
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

fn copy_path(path: &str) {
    unsafe {
        use crate::ffi::{make_nsstring, release_obj};
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pasteboard.is_null() {
            return;
        }
        let _: isize = msg_send![pasteboard, clearContents];
        let value = make_nsstring(path);
        let kind = make_nsstring("public.utf8-plain-text");
        let _: bool = msg_send![pasteboard, setString: value, forType: kind];
        release_obj(value);
        release_obj(kind);
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
