//! Executing a click the Finder extension forwarded.
//!
//! Every request is re-validated against the live config: the menu the user clicked may have been
//! built before a settings change, and a distributed notification can be posted by any local
//! process, so the message itself is never trusted to name an enabled action.

use std::path::{Path, PathBuf};

use finder_protocol::{Context, MenuAction, PathLayout};

use std::sync::atomic::{AtomicBool, Ordering};

use objc2::msg_send;
use objc2::runtime::AnyObject;

use crate::config::CONFIG;
use crate::finder_menu::{hash, new_file, terminals};
use crate::i18n::{t, t_count, tf};
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
    if !action_enabled(&config, &action) {
        log_info!("[finder-menu] ignored {action:?} while it is disabled");
        return;
    }
    match action {
        MenuAction::NewFile { extension } => {
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
            let Some(path) = paths.first() else {
                return;
            };
            copy_paths_and_report(std::slice::from_ref(path), PathLayout::Newline);
        }
        MenuAction::CopyPaths { layout } => {
            copy_paths_and_report(&paths, layout);
        }
        MenuAction::CopySha256 => {
            start_sha256(paths, false);
        }
        MenuAction::CopySha256Many { with_name } => {
            start_sha256(paths, with_name);
        }
    }
}

/// Whether the live config still enables `action`.
///
/// A menu can stay open across a settings change, and a distributed notification can name any
/// action. Everything that can produce a user-visible result — a click, a shape refusal and a
/// `ClickRefused` — goes through this one answer, so a switch turned off can never still speak
/// through one path and not another.
pub(crate) fn action_enabled(config: &crate::config::Config, action: &MenuAction) -> bool {
    let section = &config.finder_menu;
    section.enabled
        && match action {
            MenuAction::NewFile { extension } => section.new_file_types.contains(extension),
            MenuAction::OpenTerminal { bundle_id } => section.terminals.contains(bundle_id),
            MenuAction::CopyPath | MenuAction::CopyPaths { .. } => section.copy_path,
            MenuAction::CopySha256 | MenuAction::CopySha256Many { .. } => section.copy_sha256,
        }
}

/// One SHA-256 run at a time across the whole app.
///
/// Two concurrent runs would read two large selections at once and could finish out of order,
/// letting the earlier click overwrite the later result on the clipboard. A second click while one
/// is running is refused with a message rather than queued: the user can see it is busy. The flag
/// is released on the main thread **after** the clipboard write and the message, not when the
/// worker returns, so the next click cannot replace the pasteboard under a result that is still
/// being committed.
static SHA256_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn start_sha256(paths: Vec<String>, with_name: bool) {
    if paths.is_empty() {
        return;
    }
    // Build the callback target here, on the main thread, BEFORE anything is claimed. Class
    // registration panics when it fails, and a panic after the claim would stage a result that
    // no callback can ever deliver; a null target would swallow the dispatch the same way.
    if unsafe { sha256_target() }.is_null() {
        log_info!("[finder-menu] sha256 callback target unavailable");
        crate::ui::toast::show_refused(&t("finder_menu.sha256_error"));
        return;
    }
    if SHA256_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        crate::ui::toast::show_refused(&t("finder_menu.sha256_busy"));
        return;
    }
    // The claim and its guard are created together. The guard travels with the job — the worker
    // moves it into the pending result, and the main thread drops it after the clipboard write and
    // the message — so every exit from here on releases the claim exactly once: a panic while
    // showing the message, a failed spawn (the closure is dropped), a worker panic before staging,
    // a delivery failure, or the commit itself unwinding.
    let flight = Flight(&SHA256_IN_FLIGHT);
    // The computing message has no deadline: a large selection outlives any fixed timeout, and a
    // bubble that vanished early would leave the user unable to tell it is still working.
    crate::ui::toast::show_working(&t("finder_menu.sha256_working"));
    let started = std::thread::Builder::new()
        .name("finder-sha256".to_string())
        .spawn(move || {
            // A panic in the hashing path must still produce a finished state; otherwise the
            // computing bubble stays up and the claim is never released.
            let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let staged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    hash::sha256_paths(&paths)
                }));
                let pending = match staged {
                    Ok(batched) => PendingSha256::Ready {
                        batched,
                        with_name,
                        flight,
                    },
                    Err(_) => {
                        log_info!("[finder-menu] the sha256 worker panicked");
                        PendingSha256::Failed { flight }
                    }
                };
                PENDING_SHA256
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .replace(pending);
                finish_sha256_on_main();
            }));
            if dispatched.is_err() {
                // Nothing reached the main thread, so no callback will commit (and release) the
                // claim: withdraw the staged result — a later dispatch must not commit a batch
                // nobody waits for — and let the guard inside it drop.
                PENDING_SHA256
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                log_info!("[finder-menu] could not hand the sha256 batch to the main thread");
            }
        });
    if started.is_err() {
        // The failed spawn dropped the closure, and the claim guard went with it.
        log_info!("[finder-menu] could not start the sha256 worker");
        crate::ui::toast::complete_working_refused(&t("finder_menu.sha256_error"));
    }
}

/// A worker's outcome, waiting for the main thread to commit it.
///
/// Each variant owns the claim guard, so the claim cannot outlive its job or be released twice:
/// the main thread takes the variant, commits, and drops the guard at the end of the match arm.
enum PendingSha256 {
    Ready {
        batched: hash::Batched,
        with_name: bool,
        flight: Flight<'static>,
    },
    /// The worker panicked. It is reported on the main thread like any other failed run, so the
    /// "computing" bubble cannot stay up forever.
    Failed { flight: Flight<'static> },
}

/// Releases the single-flight claim when the job is committed, or dropped.
///
/// It releases on unwind too: the callback boundary catches a panic, and a claim left set would
/// make every later click report "still computing" until the app restarted.
struct Flight<'a>(&'a AtomicBool);

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The batch a worker finished with, waiting for the main thread to take it.
static PENDING_SHA256: std::sync::Mutex<Option<PendingSha256>> = std::sync::Mutex::new(None);

/// Wake the main thread to finish whatever batch is staged. Safe from any thread: it only posts.
fn finish_sha256_on_main() {
    unsafe {
        let target = sha256_target();
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: objc2::sel!(finishPendingSha256),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

static SHA256_TARGET: std::sync::OnceLock<crate::ffi::CallbackTarget> = std::sync::OnceLock::new();

unsafe fn sha256_target() -> *mut AnyObject {
    SHA256_TARGET
        .get_or_init(|| {
            let name = c"OhMyTabFinderSha256Target";
            let class = crate::ffi::StaticClass::register_once(&SHA256_CLASS, name, || {
                crate::ffi::build_class(name, objc2::class!(NSObject), |builder| {
                    builder.add_method::<AnyObject, _>(
                        objc2::sel!(finishPendingSha256),
                        finish_pending_sha256
                            as unsafe extern "C" fn(*mut AnyObject, objc2::runtime::Sel),
                    );
                })
            });
            let instance: *mut AnyObject = msg_send![class.0 as *mut AnyObject, new];
            crate::ffi::CallbackTarget::new(instance)
        })
        .0
}

static SHA256_CLASS: std::sync::OnceLock<crate::ffi::StaticClass> = std::sync::OnceLock::new();

unsafe extern "C" fn finish_pending_sha256(_this: *mut AnyObject, _cmd: objc2::runtime::Sel) {
    crate::callback_guard::void("finder_finish_sha256", || {
        // Taken, not peeked: a second dispatch for the same batch would otherwise write the
        // clipboard again after the user had already pasted it.
        let Some(pending) = PENDING_SHA256
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        else {
            return;
        };
        finish_sha256(pending);
    });
}

/// Write the batch and say what happened, on the main thread.
///
/// AppKit and the pasteboard are main-thread only, which is why the worker hands the digests here
/// instead of copying them itself. The single-flight flag is released last: the clipboard and the
/// message are part of the same commit.
fn finish_sha256(pending: PendingSha256) {
    match pending {
        // The claim is held by the pattern binding until the end of the arm, so it outlives the
        // message the arm shows and is released on unwind as well.
        PendingSha256::Failed { flight: _flight } => {
            crate::ui::toast::complete_working_refused(&t("finder_menu.sha256_error"));
        }
        PendingSha256::Ready {
            batched,
            with_name,
            flight: _flight,
        } => {
            commit_sha256(batched, with_name);
        }
    }
}

/// Write a finished batch and report it. Called on the main thread; the caller owns the claim.
fn commit_sha256(batched: hash::Batched, with_name: bool) {
    let copied = batched.entries.len();
    if copied == 0 {
        log_info!(
            "[finder-menu] sha256: no regular file in the selection ({} skipped)",
            batched.skipped
        );
        crate::ui::toast::complete_working_refused(&t("finder_menu.sha256_failed"));
    } else {
        let text = crate::finder_menu::format::digests_text(&batched.entries, with_name);
        if let Some(change_count) = copy_text(&text) {
            let message = sha256_message(copied, batched.skipped);
            log_info!(
                "[finder-menu] sha256 copied ({copied} item(s), {} skipped)",
                batched.skipped
            );
            crate::ui::toast::complete_working_done_for_pasteboard(&message, change_count);
        } else {
            log_info!("[finder-menu] sha256 could not reach the pasteboard");
            crate::ui::toast::complete_working_refused(&t("finder_menu.sha256_unavailable"));
        }
    }
}

/// What a finished batch says. One item keeps the wording it always had; a partial batch names
/// both counts, because "copied" alone would hide that some selected items are missing from the
/// clipboard.
fn sha256_message(copied: usize, skipped: usize) -> String {
    if skipped == 0 {
        if copied == 1 {
            t("finder_menu.sha256_copied")
        } else {
            tf(
                "finder_menu.sha256_count_copied",
                &[("count", &copied.to_string())],
            )
        }
    } else {
        tf(
            "finder_menu.sha256_partial",
            &[
                ("copied", &copied.to_string()),
                ("skipped", &skipped.to_string()),
            ],
        )
    }
}

/// Put `text` on the general pasteboard and return the write's change count. Reporting "copied"
/// for a failed write is worse than saying nothing, and the bubble's badge would lie; the count lets
/// a delayed confirmation verify that the write it confirms is still what the user would paste.
fn copy_text(text: &str) -> Option<i64> {
    unsafe {
        use crate::ffi::{make_nsstring, release_obj};
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let pasteboard: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pasteboard.is_null() {
            return None;
        }
        let _: isize = msg_send![pasteboard, clearContents];
        let value = make_nsstring(text);
        let kind = make_nsstring("public.utf8-plain-text");
        let written: bool = msg_send![pasteboard, setString: value, forType: kind];
        release_obj(value);
        release_obj(kind);
        if !written {
            return None;
        }
        Some(msg_send![pasteboard, changeCount])
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

/// Copy the paths and say so: the successful bubble with its count, or the refusal when the
/// pasteboard will not take the text.
fn copy_paths_and_report(paths: &[String], layout: PathLayout) {
    let text = crate::finder_menu::format::paths_text(paths, layout);
    if text.is_empty() {
        return;
    }
    let message = if paths.len() == 1 {
        t("finder_menu.path_copied")
    } else {
        t_count("finder_menu.paths_copied", paths.len())
    };
    if copy_text(&text).is_some() {
        log_info!("[finder-menu] {} path(s) copied", paths.len());
        crate::ui::toast::show_done(&message);
    } else {
        log_info!("[finder-menu] path(s) could not reach the pasteboard");
        crate::ui::toast::show_refused(&t("finder_menu.path_unavailable"));
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

#[cfg(test)]
mod tests {
    use super::Flight;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The commit runs at the edge of an Objective-C callback, where a panic is caught for us. A
    /// claim left set there would make every later click report "still computing" until the app
    /// restarted, so the release has to survive the unwind.
    #[test]
    fn the_flight_claim_is_released_when_the_commit_panics() {
        let flag = AtomicBool::new(true);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _flight = Flight(&flag);
            panic!("the commit panicked");
        }));
        assert!(panicked.is_err(), "the fixture must actually panic");
        assert!(
            !flag.load(Ordering::SeqCst),
            "the claim must be released by the guard, not by the last line of the commit"
        );
    }

    /// The SHA-256 terminal states belong to the persistent working message. Reporting them through
    /// ordinary transient feedback would let them erase (or be erased without completing) the owned
    /// progress state the toast flow keeps for the run.
    #[test]
    fn sha256_terminal_feedback_completes_the_owned_working_toast() {
        let production = include_str!("actions.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the production source precedes the test module");
        for call in [
            "complete_working_done_for_pasteboard(&message, change_count)",
            "complete_working_refused(&t(\"finder_menu.sha256_failed\"))",
            "complete_working_refused(&t(\"finder_menu.sha256_unavailable\"))",
        ] {
            assert!(
                production.contains(call),
                "the SHA-256 terminal feedback call is missing: {call}"
            );
        }
        // The spawn-failure and worker-failure branches use the same copy. Pin each branch by its
        // context so one cannot silently regress while the other keeps a broad contains() green.
        for (context, branch) in [
            (
                "could not start the sha256 worker",
                "the spawn-failure branch",
            ),
            (
                "PendingSha256::Failed { flight: _flight }",
                "the worker-failure branch",
            ),
        ] {
            let position = production
                .find(context)
                .unwrap_or_else(|| panic!("missing context for {branch}"));
            let branch_source =
                &production[position..position + 320.min(production.len() - position)];
            assert!(
                branch_source
                    .contains("complete_working_refused(&t(\"finder_menu.sha256_error\"))"),
                "{branch} must complete the owned working toast: {branch_source}"
            );
        }
    }

    /// The guard releases on a normal return too, which is the path every batch takes.
    #[test]
    fn the_flight_claim_is_released_on_a_normal_return() {
        let flag = AtomicBool::new(true);
        {
            let _flight = Flight(&flag);
            assert!(flag.load(Ordering::SeqCst), "held while the commit runs");
        }
        assert!(!flag.load(Ordering::SeqCst));
    }
}
