//! The window set behind one Dock preview panel.
//!
//! Collection is a directed Accessibility pass for one process, run on a worker thread (never on
//! the AppKit main thread: an unresponsive app would otherwise stall the panel open). It uses the
//! same collector as the switcher with a wider admission policy -- minimized, hidden and
//! other-desktop windows -- and keeps each window's identity so activation and close can be
//! re-checked against the app instance that was hovered, not against whatever holds that PID later.

use crate::app_identity;
use crate::dock_preview::decisions::WindowPresence;
use crate::window_collector::{self, MruMap};

/// One window card's data. Frames and images are added by the panel layer; this is what the
/// collector and the pure sorting rules agree on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewWindow {
    pub(crate) pid: i32,
    pub(crate) wid: u32,
    pub(crate) title: String,
    pub(crate) minimized: bool,
    pub(crate) app_hidden: bool,
    pub(crate) other_desktop: bool,
    /// The window is the app's focused one when it was collected.
    pub(crate) active: bool,
    /// At least one capture path can produce a frame for it now. A minimized window can show a
    /// frame if the cache still holds the last one; it cannot ask for a new capture.
    pub(crate) may_capture: bool,
}

/// One app's collected windows plus the process identity they belong to.
#[derive(Clone, Debug)]
pub(crate) struct CollectedWindows {
    pub(crate) pid: i32,
    pub(crate) identity_key: String,
    pub(crate) process_start_time_us: Option<u64>,
    /// The capture ledger's generation for this process at collection time. The preview binds its
    /// capture requests to it, so a window list from a dead process instance cannot capture a
    /// window of a newer instance that happens to reuse the PID.
    pub(crate) thumbnail_generation: u64,
    pub(crate) windows: Vec<PreviewWindow>,
}

/// Collect one app's windows for the Dock preview. Call from a worker thread.
pub(crate) fn collect(pid: i32) -> Option<CollectedWindows> {
    let focused_cgwid = unsafe { window_collector::focused_window_cgwid(pid) }.unwrap_or(0);
    let mut mru = MruMap::new();
    let collected =
        window_collector::collect_windows_for_dock_preview(&mut mru, pid, focused_cgwid);
    // This pass stages thumbnail capture scope on the calling thread; consume it on every path,
    // success or not, or the worker keeps its observations alive forever.
    drop(crate::thumbnail::take_current_scope_pass());

    let collected = collected?;
    let identity = unsafe { app_identity::resolve_app_identity(pid) };
    let mut windows: Vec<PreviewWindow> = collected
        .windows
        .into_iter()
        .filter(|window| window.window_id != 0)
        .map(|window| {
            let title = if window.window_title.trim().is_empty() {
                window.app_name.clone()
            } else {
                window.window_title.clone()
            };
            PreviewWindow {
                pid: window.pid,
                wid: window.window_id,
                title,
                minimized: window.minimized,
                app_hidden: window.app_hidden,
                other_desktop: window.on_other_desktop,
                active: window.is_active,
                // A capture of a minimized, hidden or off-desktop window cannot succeed; those
                // cards use a cached frame when one exists, otherwise an icon placeholder.
                may_capture: !window.minimized && !window.app_hidden && !window.on_other_desktop,
            }
        })
        .collect();
    sort_windows(&mut windows);
    Some(CollectedWindows {
        pid,
        identity_key: identity.key,
        process_start_time_us: identity.process_start_time_us,
        thumbnail_generation: crate::thumbnail::pid_generation_now(pid),
        windows,
    })
}

/// Sort the preview's cards: the app's focused window first, everything else in the collector's
/// order (which is the MRU order when history exists and a stable AX order when it does not).
/// `sort_by` is stable, so a window without history keeps its relative place.
pub(crate) fn sort_windows(windows: &mut [PreviewWindow]) {
    windows.sort_by_key(|window| std::cmp::Reverse(window.active));
}

/// Whether the process the panel was built for is still the same instance.
///
/// The PID alone is not identity (PIDs are reused), and an unreadable start time is not proof of
/// the same instance: an action that cannot establish identity is refused rather than allowed.
pub(crate) fn process_identity_matches(
    pid: i32,
    expected_key: &str,
    expected_start_time_us: Option<u64>,
) -> bool {
    if pid <= 0 || expected_key.is_empty() || !process_is_running(pid) {
        return false;
    }
    let Some(expected_start_time_us) = expected_start_time_us else {
        return false;
    };
    let current = unsafe { app_identity::resolve_app_identity(pid) };
    current.key == expected_key && current.process_start_time_us == Some(expected_start_time_us)
}

/// Whether the window still exists and still belongs to `pid`.
///
/// `Unknown` is not "gone": a failed query must not remove a card or claim a close succeeded.
pub(crate) fn window_presence(pid: i32, wid: u32) -> WindowPresence {
    match window_presence_fast(pid, wid) {
        // The on-screen layer-0 list does not carry every window (a minimized or hidden one, for
        // example). An Accessibility answer that names the window is proof enough; a missing one
        // is still unknown, never "gone". Callers that must not block the main thread use the
        // fast variant and treat Unknown as "not yet".
        WindowPresence::Unknown => match window_collector::get_ax_windows_for_pid(pid) {
            Some(windows) if windows.iter().any(|(window_id, _, _)| *window_id == wid) => {
                WindowPresence::Present
            }
            _ => WindowPresence::Unknown,
        },
        presence => presence,
    }
}

/// Presence from CoreGraphics alone: no Accessibility round trip, so the main-thread tick can
/// afford it.
pub(crate) fn window_presence_fast(pid: i32, wid: u32) -> WindowPresence {
    match window_collector::window_presence_now(wid) {
        None => WindowPresence::Unknown,
        Some((false, _)) => WindowPresence::Gone,
        Some((true, _)) => match window_collector::owner_pid_for_cgwid(wid) {
            Some(owner) if owner == pid => WindowPresence::Present,
            Some(_) => WindowPresence::Gone,
            None => WindowPresence::Unknown,
        },
    }
}

/// Whether the PID currently names a live application process.
fn process_is_running(pid: i32) -> bool {
    unsafe {
        let app: *mut objc2::runtime::AnyObject = objc2::msg_send![
            objc2::class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        if app.is_null() {
            return false;
        }
        let terminated: objc2::runtime::Bool = objc2::msg_send![app, isTerminated];
        terminated == objc2::runtime::Bool::NO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(wid: u32, active: bool, title: &str) -> PreviewWindow {
        PreviewWindow {
            pid: 7,
            wid,
            title: title.to_string(),
            minimized: false,
            app_hidden: false,
            other_desktop: false,
            active,
            may_capture: true,
        }
    }

    #[test]
    fn the_focused_window_leads_and_the_rest_keep_their_order() {
        let mut windows = vec![
            window(3, false, "third"),
            window(1, true, "focused"),
            window(2, false, "second"),
        ];
        sort_windows(&mut windows);
        let titles: Vec<&str> = windows.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, vec!["focused", "third", "second"]);
        assert_eq!(windows[0].wid, 1);
    }

    #[test]
    fn an_empty_app_has_no_cards() {
        let mut windows: Vec<PreviewWindow> = Vec::new();
        sort_windows(&mut windows);
        assert!(windows.is_empty());
    }
}
