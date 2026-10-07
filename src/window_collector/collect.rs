//! Parallelizable per-PID AX collection and result merging.

use super::space_membership::{
    query_with_provider, MembershipSnapshot, SkyLightMembershipProvider,
};
use super::*;
use crate::space_groups::MembershipScope;

/// One worker thread's partial result set for its PID chunk; merged by key afterwards --
/// the merge equals the serial version field-for-field (card order is decided by the
/// second pass over the CG array and never depends on collection order).
/// The fallback for a worker chunk that threw: every pid in it is unknown, which is the *failed*
/// state, not the empty one. Reporting them as empty would put them in `CgOnlyPairing::Reject` and
/// drop real windows from the switcher (see `cg_only_pairing`).
fn ax_partial_after_chunk_exception(chunk: &[i32]) -> AxPartial {
    AxPartial {
        icon_ids: HashMap::new(),
        hidden_app_pids: HashSet::new(),
        ax_empty_pids: HashSet::new(),
        ax_recovered_wid_to_info: HashMap::new(),
        ax_failed_pids: chunk.to_vec(),
        ax_wid_to_info: HashMap::new(),
        titleless_pids: HashSet::new(),
        ax_work_ms: 0,
    }
}

struct AxPartial {
    icon_ids: HashMap<i32, AppIdentity>,
    hidden_app_pids: HashSet<i32>,
    /// Pids whose AX query succeeded with zero standard windows. This distinguishes an empty
    /// answer from a failed query during discovery; SkyLight membership decides which Space owns
    /// any key/main recovery.
    ax_empty_pids: HashSet<i32>,
    /// Windows recovered ONLY from the key/main slots (absent from kAXWindows), kept per pid:
    /// they can recover windows AX omits on another Space; activatable-process, size, and exact
    /// Space-membership filters are applied before publication.
    ax_recovered_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>>,
    ax_failed_pids: Vec<i32>,
    ax_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>>,
    titleless_pids: HashSet<i32>,
    /// Sum of AX query work time for this chunk (diagnostics; wall clock is measured by the caller).
    ax_work_ms: u128,
}

fn should_filter_hidden_app(show_hidden_app_windows: bool, is_hidden: Option<bool>) -> bool {
    !show_hidden_app_windows && is_hidden == Some(true)
}

unsafe fn application_is_hidden(pid: i32) -> Option<bool> {
    let app: *mut AnyObject = msg_send![
        class!(NSRunningApplication),
        runningApplicationWithProcessIdentifier: pid
    ];
    (!app.is_null()).then(|| msg_send![app, isHidden])
}

/// Process a chunk of PIDs: resolve each app identity + query its AX window list. AX
/// remote messaging works from any thread (messaging timeouts are per-element);
/// resolve_app_identity only reads NSRunningApplication properties + stat. The whole
/// chunk runs inside an autoreleasepool to drain ObjC temporaries.
///
/// # Safety
/// The caller must ensure no conflicting concurrent use of shared AX/ObjC elements with
/// the main thread.
unsafe fn ax_collect_chunk(
    chunk: &[i32],
    pid_names: &HashMap<i32, String>,
    show_hidden_app_windows: bool,
) -> AxPartial {
    let mut partial = AxPartial {
        icon_ids: HashMap::new(),
        hidden_app_pids: HashSet::new(),
        ax_empty_pids: HashSet::new(),
        ax_recovered_wid_to_info: HashMap::new(),
        ax_failed_pids: Vec::new(),
        ax_wid_to_info: HashMap::new(),
        titleless_pids: HashSet::new(),
        ax_work_ms: 0,
    };
    // AppKit temporaries (NSRunningApplication et al) drain with the pool; same precedent
    // as the background icon-extraction thread.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    for &pid in chunk {
        let app_hidden = unsafe { application_is_hidden(pid) } == Some(true);
        if app_hidden {
            partial.hidden_app_pids.insert(pid);
        }
        if should_filter_hidden_app(show_hidden_app_windows, app_hidden.then_some(true)) {
            continue;
        }
        let t_pid = Instant::now();
        let identity = unsafe { resolve_app_identity(pid) };
        let process_start_time_us = identity.process_start_time_us;
        partial.icon_ids.insert(pid, identity);
        // ObjC exception boundary (AX internals in HIServices do throw NSExceptions). Such an
        // exception unwinds through Rust frames, and the thread-level catch_unwind cannot catch a
        // foreign exception (`__rust_foreign_exception` aborts the whole process; measured in the
        // 2026-09-17 22:20 crash). Caught here, that pid degrades to "no windows" with the
        // exception's name/reason logged, and the collection continues -- a process-wide crash
        // becomes one missing card.
        let ax_wins = match objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            get_ax_windows_for_pid_with_identity(pid, process_start_time_us)
        })) {
            Ok(windows) => windows,
            Err(exception) => {
                log_info!(
                    "[collect] ax exception: pid={} app=\"{}\" {:?}",
                    pid,
                    pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                    exception
                );
                None
            }
        };
        let pid_ms = t_pid.elapsed().as_millis();
        partial.ax_work_ms += pid_ms;
        // Log only slow AX queries; successful queries stay silent to avoid flooding every summon.
        // TIMING-DEBUG Flag slow AX queries (>=20ms) individually: pin down which app stalls
        // the summon.
        if pid_ms >= 20 {
            log_debug!(
                "[collect] ax slow: pid={} app=\"{}\" {}ms",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                pid_ms
            );
        }
        match ax_wins {
            Some(wins) if !wins.is_empty() => {
                // Split by origin so key/main recovery remains explicit; exact SkyLight
                // membership and normal-window filters are applied after CG/AX pairing.
                let (recovered, published): (Vec<AxWindowInfo>, Vec<AxWindowInfo>) = wins
                    .into_iter()
                    .partition(|window| window.only_via_key_or_main);
                let published = fold_tab_group(published);
                if !published.is_empty() {
                    if windows_are_all_untitled(&published) {
                        partial.titleless_pids.insert(pid);
                    }
                    partial.ax_wid_to_info.insert(pid, ax_wid_map(published));
                } else {
                    // kAXWindows cannot see any of this app's windows from the current Space.
                    partial.ax_empty_pids.insert(pid);
                    // The recovered windows get the same all-untitled exemption: an app with a
                    // custom title bar (empty titles) may expose only these, and
                    // without the exemption the empty-title gate would drop every one and the app
                    // would vanish from the list.
                    if windows_are_all_untitled(&recovered) {
                        partial.titleless_pids.insert(pid);
                    }
                }
                if !recovered.is_empty() {
                    partial
                        .ax_recovered_wid_to_info
                        .insert(pid, ax_wid_map(recovered));
                }
            }
            // AX answered with no standard windows: either the app genuinely has none that
            // Mission Control would show (e.g. an invisible anchor window), or its key/main
            // windows are the only discovery path available.
            Some(_) => {
                partial.ax_empty_pids.insert(pid);
            }
            // AX query failed (no AX data): keep the CG fallback path.
            None => partial.ax_failed_pids.push(pid),
        }
    }
    let _: () = msg_send![pool, drain];
    partial
}

/// Rediscover one PID's windows after WindowServer reports an undisplayed focused window.
/// AX supplies window identity; SkyLight membership and CG geometry constrain the result.
pub(crate) fn collect_windows_for_pid(
    mru: &mut MruMap,
    pid: i32,
    focused_cgwid: u32,
) -> Option<Vec<WindowInfo>> {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let result = collect_windows_for_pid_inner(mru, pid, focused_cgwid);
        let _: () = msg_send![pool, drain];
        result
    }
}

/// Find a switchable, capture-sized window for a frontmost app.
///
/// AX remains the authority for membership in the switcher; the CG snapshot only supplies
/// current geometry. A stale or auxiliary focused-window id therefore cannot force a thin
/// helper surface to become the prewarm target.
pub(crate) fn choose_switchable_capture_window(
    windows: &[WindowInfo],
    focused_cgwid: Option<u32>,
    preferred_cgwid: u32,
) -> Option<WindowInfo> {
    let preferred = focused_cgwid.unwrap_or(preferred_cgwid);
    windows
        .iter()
        .filter(|window| {
            !window.minimized
                // A window on another desktop cannot be captured, so it must never become a
                // prewarm/capture target even when it is the app's frontmost window.
                && !window.on_other_desktop
                && window.window_id != 0
                && window.bounds.2 >= 160.0
                && window.bounds.3 >= 120.0
        })
        .max_by_key(|window| {
            (
                window.window_id == preferred,
                (window.bounds.2 * window.bounds.3) as u64,
            )
        })
        .cloned()
}

pub(crate) fn switchable_capture_window_for_pid(
    pid: i32,
    preferred_cgwid: u32,
) -> Option<WindowInfo> {
    let focused_cgwid = unsafe { focused_window_cgwid(pid) };
    let mut mru = MruMap::new();
    let collected =
        collect_windows_for_pid(&mut mru, pid, focused_cgwid.unwrap_or(preferred_cgwid));
    // This caller only wants one window to capture, not the pass's scope observations: consume and
    // drop them on *every* path, including the failure return below, or an idle prewarm thread would
    // keep its last pass -- and with it the oldest destruction epoch -- alive forever.
    drop(crate::thumbnail::take_current_scope_pass());
    let windows = collected?;
    choose_switchable_capture_window(&windows, focused_cgwid, preferred_cgwid)
}

unsafe fn collect_windows_for_pid_inner(
    mru: &mut MruMap,
    pid: i32,
    focused_cgwid: u32,
) -> Option<Vec<WindowInfo>> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    let show_hidden_app_windows = CONFIG.read().unwrap().windows.show_hidden_app_windows;
    let show_other_desktops = CONFIG.read().unwrap().windows.show_other_desktops;
    // See the full-collection entry: staging is per pass and published only on acceptance.
    crate::thumbnail::begin_scope_pass();
    let _identity_pass = begin_ax_identity_pass();
    let app_hidden = unsafe { application_is_hidden(pid) } == Some(true);
    if should_filter_hidden_app(show_hidden_app_windows, app_hidden.then_some(true)) {
        return Some(Vec::new());
    }
    let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
    if array.is_null() {
        return None;
    }
    let display_bounds = active_display_bounds();

    let identity = resolve_app_identity(pid);
    let Some(ax_wins) = get_ax_windows_for_pid_with_identity(pid, identity.process_start_time_us)
    else {
        CFRelease(array);
        return None;
    };
    // A directed (single-app) refresh honours only the kAXWindows answer: windows recovered from
    // the key/main slots are not Space-filtered and are often helper overlays, so they must not
    // enter the list through this path. When the answer consists solely of recovered windows it
    // counts as unanswered (None), so the caller keeps the existing cards instead of losing them.
    let mut published: Vec<AxWindowInfo> = Vec::with_capacity(ax_wins.len());
    let mut recovered_only = !ax_wins.is_empty();
    for window in ax_wins {
        if window.only_via_key_or_main {
            continue;
        }
        recovered_only = false;
        published.push(window);
    }
    if recovered_only {
        CFRelease(array);
        return None;
    }
    let published = fold_tab_group(published);
    let ax_wid_to_info: HashMap<u32, AxWindowInfo> = published
        .iter()
        .filter_map(|window| (window.cgwid != 0).then_some((window.cgwid, window.clone())))
        .collect();
    let titleless = !published.is_empty() && published.iter().all(|window| window.title.is_empty());
    let icon_path = check_cache_for_identity(&identity);
    let last_activated = LAST_ACTIVATED.lock().unwrap().get(&pid).copied();
    let now = Instant::now();
    let ancient_base = now.checked_sub(Duration::from_secs(86_400)).unwrap_or(now);
    let mut insertion_order = 0;
    let mut app_name = String::new();
    let mut shown = HashSet::new();
    let mut current_cg_ids = HashSet::new();
    let mut fullscreen_cgwids = HashSet::new();
    let mut cg_window_layers: HashMap<u32, i32> = HashMap::new();
    // The AX-only backfill needs this pass's shape for a window it may restore, so the bounds are
    // recorded before the admission filters rather than where the pairing stage uses them.
    let mut cg_window_bounds: HashMap<u32, (f64, f64, f64, f64)> = HashMap::new();
    let mut windows = Vec::new();
    let count = CFArrayGetCount(array);
    let mut cg_window_ids = Vec::new();
    for i in 0..count {
        let dict = CFArrayGetValueAtIndex(array, i);
        if !dict.is_null() {
            let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
            let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
            if owner_pid == pid && cgwid != 0 {
                cg_window_ids.push(cgwid);
            }
        }
    }
    // The directed pass must publish its own evidence for this pid -- including "no published /
    // no recovered windows" -- or a scenario would explain its new cards with an older pass's
    // conditions. It serves one pid and deliberately ignores key/main-only recovery, so its
    // recovered set is empty. Staged here, taken with the result, published only on acceptance.
    crate::e2e_state::stage_ax_pid_evidence(
        (!ax_wid_to_info.is_empty()).then_some(pid),
        std::iter::empty::<i32>(),
        std::iter::empty::<i32>(),
    );
    let mut membership_window_ids = cg_window_ids.clone();
    membership_window_ids.extend(ax_wid_to_info.keys().copied());
    membership_window_ids.sort_unstable();
    membership_window_ids.dedup();
    let (membership_source, membership_snapshot) =
        query_space_membership(&membership_window_ids, false);
    let parent_ids: HashMap<u32, u32> = skylight::window_parent_ids(&cg_window_ids);
    let focused_cgwid = parent_ids
        .get(&focused_cgwid)
        .copied()
        .filter(|parent| *parent != 0)
        .unwrap_or(focused_cgwid);

    for i in 0..count {
        let dict = CFArrayGetValueAtIndex(array, i);
        if dict.is_null() {
            continue;
        }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid != pid {
            continue;
        }
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        // Staged before the attached-surface, alpha and AX-pairing filters below, for the same
        // reason as the full pass: the observation is about the window, not about its card.
        let staged_scope = classify_window_space(
            membership_source,
            membership_snapshot.as_ref(),
            WindowPairingSource::PublishedAx,
            cgwid,
            cf_dict_get_bool(dict, "kCGWindowIsOnscreen"),
        );
        if staged_scope != MembershipScope::Unknown {
            crate::thumbnail::stage_window_scope(
                pid,
                cgwid,
                staged_scope == MembershipScope::OtherDesktop,
            );
        }
        if cgwid != 0 {
            cg_window_layers.insert(cgwid, layer);
            if let Some(bounds) = cf_dict_get_bounds(dict, "kCGWindowBounds") {
                cg_window_bounds.insert(cgwid, bounds);
            }
        }
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        if cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0) <= 0.0 {
            continue;
        }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }
        if cgwid != 0 {
            current_cg_ids.insert(cgwid);
        }
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));
        let cg_is_onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen");
        // The directed refresh honours the kAXWindows answer... except for a window Space
        // membership places on another desktop, which that answer cannot contain (it is filtered
        // by the current Space). This is the same exception the full collection makes; without it
        // a per-app refresh would drop a card the summon had just shown.
        let (ax_info, pairing_source) = match ax_wid_to_info.get(&cgwid) {
            Some(info) => (Some(info), WindowPairingSource::PublishedAx),
            // The same app-level rule the full collection applies: a window this app's AX answer
            // did not identify may only come in through the other-desktop exception, which needs
            // the app to have named a window. A failed query never reaches here (it keeps the
            // existing cards above), and an app whose answer named nothing has no switchable window
            // to show -- admitting its CG entries would let a menu-bar panel back in on a refresh
            // (see `cg_only_pairing`).
            None => match cg_only_pairing(
                !ax_wid_to_info.is_empty(),
                false,
                false,
                cg_is_onscreen.unwrap_or(false),
            ) {
                CgOnlyPairing::OtherDesktop
                    if admits_other_desktop_cg_window(
                        membership_source,
                        membership_snapshot.as_ref(),
                        cgwid,
                        show_other_desktops,
                    ) && cg_only_window_admissible(layer, bounds) =>
                {
                    (None, WindowPairingSource::UnpublishedAx)
                }
                _ => continue,
            },
        };
        if pairing_source == WindowPairingSource::PublishedAx {
            remember_ax_identity(pid, identity.process_start_time_us, cgwid);
        }
        let is_main = ax_info.is_some_and(|info| info.is_main);
        let is_floating_window = ax_info.is_some_and(|info| info.is_floating_window);
        let native_fullscreen = ax_info.is_some_and(|info| {
            native_fullscreen_state(info.is_fullscreen, bounds, &display_bounds)
        });
        if native_fullscreen {
            fullscreen_cgwids.insert(cgwid);
        }
        // The layer rule needs positive AX evidence of fullscreen: the bounds fallback is also true
        // of a display-sized floating overlay (see `ax_reports_fullscreen`). A titled
        // AXFloatingWindow additionally needs the ordinary-window shape, because its subrole
        // follows the presentation state (see AxWindowInfo::is_floating_window).
        if !window_admission(
            layer,
            is_main,
            ax_info.and_then(|info| info.is_fullscreen),
            is_floating_window,
            bounds,
        ) {
            continue;
        }
        if ax_info.is_some_and(|info| info.is_custom_root)
            && !custom_window_is_substantial(bounds)
            && !is_main
        {
            continue;
        }
        // Same sticky rejection as the full pass: a window the full collection refuses must not be
        // restored by a directed refresh, or a card would come back the moment its app activates.
        if is_known_non_normal_window(pid, identity.process_start_time_us, cgwid) && !is_main {
            continue;
        }
        if pid == std::process::id() as i32 && !cg_is_onscreen.unwrap_or(false) {
            continue;
        }
        // Without an AX element the title comes from the CG window name (which macOS only exposes
        // with Screen Recording) and the minimized state is unknowable, so it reads false.
        let window_title = ax_info
            .map(|info| info.title.clone())
            .unwrap_or_else(|| cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default());
        let minimized = ax_info.is_some_and(|info| info.minimized);
        let space_scope = classify_window_space(
            membership_source,
            membership_snapshot.as_ref(),
            pairing_source,
            cgwid,
            cg_is_onscreen,
        );
        if !admits_space_scope(
            space_scope,
            WindowSpacePolicy {
                minimized,
                show_minimized,
                show_other_desktops,
            },
        ) {
            crate::e2e_state::space_gate_rejected();
            continue;
        }
        if space_scope == MembershipScope::OtherDesktop {
            crate::e2e_state::other_desktop_accepted();
        }
        // Title and shape: both follow the classification, so the AX-query-failed and key/main
        // routes cannot slip an untitled or helper-shaped window in past the published-AX one. The
        // app-level titleless exemption never applies to another desktop's window.
        let titleless_exempt = titleless_exemption_applies(space_scope, pairing_source, titleless);
        if window_title.is_empty() && !titleless_exempt {
            continue;
        }
        if space_scope == MembershipScope::OtherDesktop
            && !other_desktop_candidate_admissible(layer, bounds, &window_title)
        {
            crate::e2e_state::space_gate_rejected();
            continue;
        }

        initialize_window_mru(
            mru,
            pid,
            cgwid,
            last_activated,
            ancient_base,
            insertion_order,
        );
        insertion_order += 1;
        app_name = owner_name;
        windows.push(WindowInfo {
            pid,
            window_id: cgwid,
            app_name: app_name.clone(),
            window_title,
            icon_path: icon_path.clone(),
            is_active: false,
            minimized,
            app_hidden,
            fullscreen: native_fullscreen,
            on_other_desktop: space_scope == MembershipScope::OtherDesktop,
            bounds,
        });
        shown.insert(cgwid);
    }
    remember_non_normal_cg_windows_for_process(
        pid,
        identity.process_start_time_us,
        &cg_window_layers,
        &ax_wid_to_info,
        &fullscreen_cgwids,
        &parent_ids,
    );
    CFRelease(array);

    // AX-only windows remain valid, for example orderOut'd settings dialogs absent from CG.
    if pid != std::process::id() as i32 {
        for (&cgwid, ax_info) in &ax_wid_to_info {
            if shown.contains(&cgwid) {
                continue;
            }
            if is_attached_surface(parent_ids.get(&cgwid).copied()) {
                continue;
            }
            // The sticky classification is checked first: it decides without asking the WindowServer
            // anything, and a window already known to be non-normal is dropped whether or not this
            // pass knows its layer.
            let sticky = is_known_non_normal_window(pid, identity.process_start_time_us, cgwid);
            let layer = if sticky {
                None
            } else {
                ax_only_layer_for_backfill(cg_window_layers.get(&cgwid).copied(), || {
                    window_layer_now(cgwid)
                })
            };
            if !backfill_admission(
                layer,
                cg_window_bounds.get(&cgwid).copied(),
                sticky,
                ax_info.is_custom_root,
                ax_info.is_main,
                ax_info.is_floating_window,
            ) {
                continue;
            }
            if ax_info.title.is_empty() && !titleless {
                continue;
            }
            let native_fullscreen = ax_info.is_fullscreen == Some(true);
            let backfill_scope = classify_window_space(
                membership_source,
                membership_snapshot.as_ref(),
                WindowPairingSource::PublishedAx,
                cgwid,
                None,
            );
            if !admits_space_scope(
                backfill_scope,
                WindowSpacePolicy {
                    minimized: ax_info.minimized,
                    show_minimized,
                    show_other_desktops,
                },
            ) {
                crate::e2e_state::space_gate_rejected();
                continue;
            }
            if backfill_scope == MembershipScope::OtherDesktop {
                crate::e2e_state::other_desktop_accepted();
            }
            // This path publishes an AX-reported window, so record the identity: a later pass whose
            // Space-filtered answer cannot name it must still know AX identified it.
            remember_ax_identity(pid, identity.process_start_time_us, cgwid);
            // The backfill restores windows the CG pass did not publish, so it must obey the same
            // classification-based rules or a window the CG path just refused comes back here.
            // Geometry evidence is required for an "other desktop" candidate: without a CG entry we
            // cannot tell an ordinary window from a helper surface, and a cross-desktop window does
            // have a CG entry (the All-mode enumeration lists off-Space windows).
            let titleless_exempt = titleless_exemption_applies(
                backfill_scope,
                WindowPairingSource::PublishedAx,
                titleless,
            );
            if ax_info.title.is_empty() && !titleless_exempt {
                continue;
            }
            if backfill_scope == MembershipScope::OtherDesktop
                && !(layer == Some(0)
                    && cg_window_bounds
                        .get(&cgwid)
                        .copied()
                        .is_some_and(custom_window_is_substantial)
                    && !ax_info.title.is_empty())
            {
                crate::e2e_state::space_gate_rejected();
                continue;
            }
            initialize_window_mru(
                mru,
                pid,
                cgwid,
                last_activated,
                ancient_base,
                insertion_order,
            );
            insertion_order += 1;
            windows.push(WindowInfo {
                pid,
                window_id: cgwid,
                app_name: app_name.clone(),
                window_title: ax_info.title.clone(),
                icon_path: icon_path.clone(),
                is_active: false,
                minimized: ax_info.minimized,
                app_hidden,
                fullscreen: native_fullscreen,
                on_other_desktop: backfill_scope == MembershipScope::OtherDesktop,
                bounds: (0.0, 0.0, 0.0, 0.0),
            });
        }
    }

    if app_name.is_empty() {
        let app: *mut AnyObject = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        app_name = crate::ffi::ns_running_app_name(app);
        if app_name.is_empty() {
            app_name = format!("PID {pid}");
        }
        for window in &mut windows {
            window.app_name = app_name.clone();
        }
    }

    // Directed collection prunes dead windows only for the target PID; it must not prune other PIDs.
    // Preserve MRU for known windows still alive in CG when AX transiently omits them; otherwise
    // they return as "new" windows and lose their ordering history.
    let live_ids: HashSet<u32> = current_cg_ids
        .into_iter()
        .chain(ax_wid_to_info.keys().copied().filter(|cgwid| {
            !is_attached_surface(parent_ids.get(cgwid).copied())
                && (should_backfill_ax_window_for_process(
                    pid,
                    *cgwid,
                    identity.process_start_time_us,
                    cg_window_layers.get(cgwid).copied(),
                ) || is_known_non_normal_window(pid, identity.process_start_time_us, *cgwid))
        }))
        .collect();
    mru.retain(|(entry_pid, window_id), _| *entry_pid != pid || live_ids.contains(window_id));
    sort_windows_by_mru(&mut windows, mru, now);
    for window in &mut windows {
        window.is_active = window.window_id == focused_cgwid;
    }
    Some(windows)
}

pub fn collect_windows(mru: &mut MruMap) -> Vec<WindowInfo> {
    collect_windows_with_frontmost_bump(mru, true)
}

// Same 16pt edge tolerance used by window_management snap-state inference.
const FULL_DISPLAY_BOUNDS_EPSILON: f64 = 16.0;

fn active_display_bounds() -> Vec<(f64, f64, f64, f64)> {
    unsafe {
        let mut count = 0;
        if CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count) != 0 || count == 0 {
            return Vec::new();
        }
        let mut displays = vec![0u32; count as usize];
        let mut returned_count = count;
        if CGGetActiveDisplayList(count, displays.as_mut_ptr(), &mut returned_count) != 0 {
            return Vec::new();
        }
        let returned_count = returned_count.min(displays.len() as u32) as usize;
        displays.truncate(returned_count);
        displays
            .into_iter()
            .map(|display| {
                let bounds = CGDisplayBounds(display);
                (bounds.x, bounds.y, bounds.w, bounds.h)
            })
            .collect()
    }
}

fn bounds_match_full_display(window: (f64, f64, f64, f64), display: (f64, f64, f64, f64)) -> bool {
    let (x, y, width, height) = window;
    let (display_x, display_y, display_width, display_height) = display;
    [
        x,
        y,
        width,
        height,
        display_x,
        display_y,
        display_width,
        display_height,
    ]
    .iter()
    .all(|value| value.is_finite())
        && width > 0.0
        && height > 0.0
        && display_width > 0.0
        && display_height > 0.0
        && (x - display_x).abs() <= FULL_DISPLAY_BOUNDS_EPSILON
        && (y - display_y).abs() <= FULL_DISPLAY_BOUNDS_EPSILON
        && (x + width - (display_x + display_width)).abs() <= FULL_DISPLAY_BOUNDS_EPSILON
        && (y + height - (display_y + display_height)).abs() <= FULL_DISPLAY_BOUNDS_EPSILON
}

fn cg_bounds_identify_native_fullscreen(
    bounds: (f64, f64, f64, f64),
    displays: &[(f64, f64, f64, f64)],
) -> bool {
    let (x, y, width, height) = bounds;
    if ![x, y, width, height].iter().all(|value| value.is_finite()) || width <= 0.0 || height <= 0.0
    {
        return false;
    }
    let center_x = x + width / 2.0;
    let center_y = y + height / 2.0;
    displays.iter().copied().any(|display| {
        let (display_x, display_y, display_width, display_height) = display;
        center_x >= display_x
            && center_x <= display_x + display_width
            && center_y >= display_y
            && center_y <= display_y + display_height
            && bounds_match_full_display(bounds, display)
    })
}

fn native_fullscreen_state(
    ax_fullscreen_from_ax: Option<bool>,
    bounds: (f64, f64, f64, f64),
    displays: &[(f64, f64, f64, f64)],
) -> bool {
    ax_fullscreen_from_ax == Some(true) || cg_bounds_identify_native_fullscreen(bounds, displays)
}

/// Whether AX positively reports the window as a native fullscreen presentation.
///
/// This -- not `native_fullscreen_state` -- is the evidence the layer rule may use. The bounds
/// fallback answers "does this surface look full-screen", which is also true of a display-sized
/// floating overlay: Telegram's media viewer sits at layer 101 with `AXFullScreen = false` and
/// bounds equal to the display rect, and treating its size as fullscreen evidence admitted it as a
/// switcher card. The fallback stays in use for the card's presentation flag and the
/// space-transition observer, where size is the question being asked.
pub(super) fn ax_reports_fullscreen(ax_fullscreen: Option<bool>) -> bool {
    ax_fullscreen == Some(true)
}

fn native_fullscreen_cg_window_ids(
    cg_bounds: &HashMap<(i32, u32), (f64, f64, f64, f64)>,
    published: &HashMap<i32, HashMap<u32, AxWindowInfo>>,
    recovered: &HashMap<i32, HashMap<u32, AxWindowInfo>>,
    displays: &[(f64, f64, f64, f64)],
) -> HashSet<(i32, u32)> {
    cg_bounds
        .iter()
        .filter_map(|(&(pid, cgwid), &bounds)| {
            let ax_info = published
                .get(&pid)
                .and_then(|windows| windows.get(&cgwid))
                .or_else(|| recovered.get(&pid).and_then(|windows| windows.get(&cgwid)))?;
            native_fullscreen_state(ax_info.is_fullscreen, bounds, displays).then_some((pid, cgwid))
        })
        .collect()
}

fn recovered_window_passes_size_filter(bounds: (f64, f64, f64, f64)) -> bool {
    custom_window_is_substantial(bounds)
}

/// The all-untitled exemption test (pure; unit-tested). At least one window is required: an
/// empty set must not qualify, or "saw no windows" would read as "untitled windows".
pub(super) fn windows_are_all_untitled(windows: &[AxWindowInfo]) -> bool {
    !windows.is_empty() && windows.iter().all(|window| window.title.is_empty())
}

/// Index a batch of AX window facts by CGWindowID (entries with cgwid == 0 have no window to pair
/// with and are dropped).
fn ax_wid_map(windows: Vec<AxWindowInfo>) -> HashMap<u32, AxWindowInfo> {
    let mut map = HashMap::new();
    for window in windows {
        if window.cgwid != 0 {
            map.insert(window.cgwid, window);
        }
    }
    map
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowPairingSource {
    /// The app's `kAXWindows` answer named this window: full AX evidence.
    PublishedAx,
    /// Recovered from the key/main slots; discovery only, and never Space-filtered by AX.
    RecoveredAx,
    /// AX could not answer for this app at all.
    AxUnavailable,
    /// AX answered for this app but did not publish this window, which Space membership places
    /// on another desktop. Kept distinct from `AxUnavailable` because the missing AX evidence is
    /// explained, not absent: there is no AX element to reinterpret (no title, no minimized
    /// state, no raise element), only the CGWindowList facts and the membership that admitted it.
    UnpublishedAx,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MembershipSource {
    SkyLight,
    Legacy,
}

static SKYLIGHT_MEMBERSHIP_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
fn select_membership_source(
    force_legacy: bool,
    skylight_query_succeeded: bool,
) -> MembershipSource {
    if force_legacy || !skylight_query_succeeded {
        MembershipSource::Legacy
    } else {
        MembershipSource::SkyLight
    }
}

fn query_space_membership(
    window_ids: &[u32],
    complete_observation: bool,
) -> (MembershipSource, Option<MembershipSnapshot>) {
    // `dev_flags::enabled` prepends `--` itself, so the name must be bare: passing
    // "--space-membership-legacy" searched for "----space-membership-legacy" and never matched,
    // leaving this switch silently dead.
    let force_legacy = crate::dev_flags::enabled("space-membership-legacy");
    let group_generation = crate::space_groups::with_tracker(|tracker| tracker.generation());
    let started = Instant::now();
    let result = if force_legacy {
        None
    } else {
        Some(query_with_provider(&SkyLightMembershipProvider, window_ids))
    };
    let snapshot = result.as_ref().and_then(|result| result.as_ref().ok());
    let source = select_membership_source(force_legacy, snapshot.is_some());
    crate::e2e_state::set_space_membership_source(source == MembershipSource::SkyLight);
    if let Some(snapshot) = snapshot {
        let (committed, current_spaces_changed) =
            crate::space_groups::with_tracker_mut(|tracker| {
                let current_spaces_changed = complete_observation
                    && tracker.topology().current_spaces_differ(&snapshot.topology);
                let committed = tracker.observe_topology_if_generation(
                    group_generation,
                    snapshot.topology.clone(),
                    complete_observation,
                );
                (committed, committed && current_spaces_changed)
            });
        if current_spaces_changed {
            crate::window_refresh::note_space_transition(false);
        }
        if !committed {
            log_debug!(
                "[collect] discarded stale Space topology snapshot generation={}",
                group_generation
            );
        }
        log_debug!(
            "[collect] skylight membership: windows={} current_spaces={} elapsed_ms={}",
            window_ids.len(),
            snapshot.current_space_ids.len(),
            started.elapsed().as_millis()
        );
    } else if !force_legacy {
        if let Some(Err(error)) = result.as_ref() {
            if !SKYLIGHT_MEMBERSHIP_FALLBACK_LOGGED.swap(true, Ordering::Relaxed) {
                log_info!(
                    "SkyLight window Space membership unavailable ({:?}); using legacy Space filter",
                    error
                );
            }
        }
    }
    (source, snapshot.cloned())
}

/// Classify one window's Space evidence without applying any window-state option.
///
/// SkyLight answers with the window's actual memberships; the accepted topology then decides
/// whether those name the current group of some display, a managed Space outside it (another
/// desktop), or nothing usable. The legacy source has no membership data at all, so its only
/// accepted evidence -- a positively onscreen window -- is classified as the current group;
/// windows that are merely offscreen stay `Unknown` rather than being guessed onto another
/// desktop.
fn classify_window_space(
    source: MembershipSource,
    snapshot: Option<&MembershipSnapshot>,
    pairing_source: WindowPairingSource,
    cgwid: u32,
    is_onscreen: Option<bool>,
) -> MembershipScope {
    match source {
        MembershipSource::SkyLight => {
            let snapshot = snapshot.expect("SkyLight source requires a membership snapshot");
            let Some(memberships) = snapshot.window_space_ids.get(&cgwid) else {
                return MembershipScope::Unknown;
            };
            crate::space_groups::with_tracker(|tracker| {
                // An empty tracker topology means no query has been accepted yet; the snapshot's
                // own topology is then the best available evidence (same fallback as before).
                let topology = if tracker.topology().displays.is_empty() {
                    &snapshot.topology
                } else {
                    tracker.topology()
                };
                topology.membership_scope(memberships, tracker.fullscreen_origins())
            })
        }
        MembershipSource::Legacy => {
            if passes_legacy_space_gate(pairing_source, is_onscreen) {
                MembershipScope::CurrentGroup
            } else {
                MembershipScope::Unknown
            }
        }
    }
}

#[derive(Clone, Copy)]
struct WindowSpacePolicy {
    minimized: bool,
    show_minimized: bool,
    // Widens the candidate scope to managed Spaces outside every display's current group. Read
    // per collection like the other switches; it never changes Space ownership or the raise target.
    show_other_desktops: bool,
}

/// Apply the window-state options to a classification.
///
/// The minimized option controls admission, never Space ownership, so it is checked first and
/// applies to every scope. `OtherDesktop` is admitted only while the "other desktops" switch is
/// on; `Unknown` is never admitted by either switch.
fn admits_space_scope(scope: MembershipScope, policy: WindowSpacePolicy) -> bool {
    if policy.minimized && !policy.show_minimized {
        return false;
    }
    match scope {
        MembershipScope::CurrentGroup => true,
        MembershipScope::OtherDesktop => policy.show_other_desktops,
        MembershipScope::Unknown => false,
    }
}

/// The whole Space decision in one call. Production sites need the classification on its own
/// (to publish `on_other_desktop` and to relax the AX pairing), so this composition is the shape
/// the tests assert against.
#[cfg(test)]
fn passes_space_policy(
    source: MembershipSource,
    snapshot: Option<&MembershipSnapshot>,
    pairing_source: WindowPairingSource,
    cgwid: u32,
    is_onscreen: Option<bool>,
    policy: WindowSpacePolicy,
) -> bool {
    admits_space_scope(
        classify_window_space(source, snapshot, pairing_source, cgwid, is_onscreen),
        policy,
    )
}

/// Whether the app-level "all of its windows are untitled" answer may excuse an empty title.
///
/// It never covers a candidate admitted as a window of another desktop, whatever pairing source
/// carried it: that answer describes the windows AX could see on *this* desktop and cannot vouch for
/// a window of another one -- neither for a window AX never published, nor for one recovered from the
/// key/main slots. Such a candidate has only its CG window name, so without one it has no title at
/// all. The current group keeps the pre-existing exemption.
fn titleless_exemption_applies(
    scope: MembershipScope,
    pairing_source: WindowPairingSource,
    app_all_untitled: bool,
) -> bool {
    scope == MembershipScope::CurrentGroup
        && pairing_source != WindowPairingSource::UnpublishedAx
        && app_all_untitled
}

/// Every extra rule a candidate admitted as a window of another desktop must satisfy on top of the
/// existing filters.
///
/// Such a window has no AX element, so the AX judgments that normally admit a custom-root or
/// titled-floating window are unavailable: the ordinary layer-0, ordinary-size shape is required
/// instead, and the title must be its own. Applied to the *classification* rather than to one
/// pairing source, so the AX-query-failed and key/main-recovered routes cannot slip a helper surface
/// or an untitled window in. Pure function, unit-tested.
fn other_desktop_candidate_admissible(
    layer: i32,
    bounds: (f64, f64, f64, f64),
    title: &str,
) -> bool {
    cg_only_window_admissible(layer, bounds) && !title.is_empty()
}

/// What to do with a CG window the app's AX answer did not identify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CgOnlyPairing {
    /// Consider it through the other-desktop exception (the caller still applies the membership
    /// and shape gates).
    OtherDesktop,
    /// Keep the CG fallback for an app whose AX query failed.
    AxUnavailable,
    /// Not one of the app's windows.
    Reject,
}

/// Decide the path for a CG window AX did not identify.
///
/// The observed case: Stats keeps a 280x800 window titled "Combined modules" -- its menu-bar panel,
/// closed and off screen, layer 0, no parent -- and answers *nothing* to AX while it is closed
/// (`kAXWindows` empty, `AXFocusedWindow`/`AXMainWindow` unsupported). Two rules let it through:
/// the other-desktop exception, and the CG fallback for "no AX data" -- which also covered "AX
/// answered about this app and named no window", so the panel became a card on its own desktop too.
/// Selecting it does nothing.
///
/// The exception exists because `kAXWindows` is Space-filtered, so a real window of another desktop
/// is absent from it by construction -- but that reasoning needs the app to have *some* window AX
/// can name, in its published list or through its key/main slots. An app AX answered about with
/// nothing at all has no switchable window to show, and its CG entries are panels.
///
/// A *failed* read is a different state: nothing is known. The fallback then covers only a window we
/// can see (`on_screen`), which is what makes it useful for a hung app -- and, by construction,
/// excludes a closed panel and everything on another desktop. Measured 2026-10-07: freezing Stats so
/// its AX read times out put it in `ax_failed_pids` and the fallback admitted both its visible
/// settings window and the closed panel, which is exactly the pair the user saw.
/// Pure function, unit-tested.
fn cg_only_pairing(
    published_any: bool,
    recovered_any: bool,
    query_failed: bool,
    on_screen: bool,
) -> CgOnlyPairing {
    if published_any || recovered_any {
        CgOnlyPairing::OtherDesktop
    } else if query_failed && on_screen {
        CgOnlyPairing::AxUnavailable
    } else {
        CgOnlyPairing::Reject
    }
}

/// Whether AX's evidence for this app excludes this particular CG window.
///
/// The observed case: 微信 keeps a 280x380 off-screen window titled 微信 beside its real 1097x833
/// window. `kAXWindows` never lists it, so on the app's own desktop the AX-authority pairing drops
/// it -- but from another desktop that list is Space-filtered (empty), the key/main slots still
/// identify the real window, and the CG-only fallback turned the helper into a second, dead card
/// (no AX element, so raising it does nothing).
///
/// The shape to catch is therefore: the published list is empty, the key/main slots named *some*
/// window of this app, and this window is neither in the recovered map (that arm matches first) nor
/// ever identified by AX before. `ever_identified` is what keeps a real second window of the app
/// admissible: once its desktop has been visited, AX has identified it and it is no longer "a window
/// AX refuses to name".
///
/// Pure function, unit-tested.
fn ax_evidence_excludes_this_window(
    published_any: bool,
    recovered_count: usize,
    ever_identified: bool,
) -> bool {
    !published_any && recovered_count > 0 && !ever_identified
}

/// Whether a CG window that the app's AX answer did not publish may still enter as a window of
/// another desktop.
///
/// This is deliberately narrow. It needs the switch, SkyLight membership, and accepted-topology
/// evidence that the window is managed *and* outside every display's current group; a window the
/// current group already covers keeps today's AX authority (the AX list, not this exception,
/// decides whether it is switchable), and `Unknown` membership is never evidence. That last part
/// is what keeps orderOut'd and helper surfaces out: they present an empty or unmanaged Space
/// list, not a managed desktop of their own.
fn admits_other_desktop_cg_window(
    source: MembershipSource,
    snapshot: Option<&MembershipSnapshot>,
    cgwid: u32,
    show_other_desktops: bool,
) -> bool {
    show_other_desktops
        && source == MembershipSource::SkyLight
        && classify_window_space(
            source,
            snapshot,
            WindowPairingSource::UnpublishedAx,
            cgwid,
            None,
        ) == MembershipScope::OtherDesktop
}

/// Legacy Space policy retained for OS updates where SkyLight membership cannot be queried.
fn passes_legacy_space_gate(_source: WindowPairingSource, is_onscreen: Option<bool>) -> bool {
    is_onscreen == Some(true)
}

/// Folds the AX windows that belong to one native tab group down to a single card: the window
/// exposing the tab bar (the selected tab) survives, and background-tab windows whose titles map
/// one-to-one onto its tab titles are dropped; any other window (a title that matches no tab) is
/// an independent window and is kept.
///
/// Why: every tab of a native tab group is its own window, but AX hands them all back ONLY while
/// the group is minimized (the visible/hidden states hand over the selected tab alone). Without
/// folding, one window becomes several cards once minimized, disagreeing with the other states.
///
/// Why "one title per background tab, bounded by the tab count" rather than "the whole AX list
/// must equal the tab set": the strict form gives up on the common mixed case (a tab group and a
/// separate window both minimized), while this one still folds the real tabs. Safety comes from
/// the bound: a group has at most titles.len()-1 background tabs, so more matches than that mean
/// the list holds a non-tab window sharing a tab's title, and nothing can say which is which --
/// keep everything. An independent window whose title matches no tab (the usual case) is never
/// matched and can never be hidden.
pub(super) fn fold_tab_group(windows: Vec<AxWindowInfo>) -> Vec<AxWindowInfo> {
    let hosts: Vec<usize> = windows
        .iter()
        .enumerate()
        .filter(|(_, window)| {
            window
                .tab_group
                .as_ref()
                .is_some_and(|group| group.titles.len() >= 2)
        })
        .map(|(index, _)| index)
        .collect();
    // Exactly one window exposing a tab bar is an unambiguous group; 0 has nothing to fold and
    // >=2 means several groups or an inconsistent read.
    if hosts.len() != 1 {
        return windows;
    }
    let host_index = hosts[0];
    let Some(titles) = windows[host_index]
        .tab_group
        .as_ref()
        .map(|group| group.titles.clone())
    else {
        return windows;
    };
    // Give every non-target window an unused slot among the tab titles. Only a window that claims a
    // slot and is minimized is a candidate background tab; the rest stay independent windows.
    let mut remaining: Vec<&str> = titles.iter().map(String::as_str).collect();
    let mut siblings = HashSet::new();
    for (index, window) in windows.iter().enumerate() {
        if index == host_index || !window.minimized {
            continue;
        }
        if let Some(position) = remaining.iter().position(|title| *title == window.title) {
            remaining.swap_remove(position);
            siblings.insert(index);
        }
    }
    // A tab group has at most titles.len()-1 background tabs. Matching more than that means the list
    // holds non-tab windows that share a tab's title, so which ones are tabs cannot be decided: keep
    // them all.
    if siblings.len() > titles.len() - 1 {
        return windows;
    }
    windows
        .into_iter()
        .enumerate()
        .filter_map(|(index, window)| (!siblings.contains(&index)).then_some(window))
        .collect()
}

/// Collect a window snapshot, optionally recording the current frontmost window in MRU.
/// Lifecycle-triggered refreshes only update the window set and must not act like a summon.
pub(crate) fn collect_windows_with_frontmost_bump(
    mru: &mut MruMap,
    bump_frontmost: bool,
) -> Vec<WindowInfo> {
    let collection_started_at = Instant::now();
    // This pass owns its scope observations; the caller carries the returned id so only an accepted
    // pass is ever published (see the main thread apply and `WindowRefreshResult`).
    crate::thumbnail::begin_scope_pass();
    let _identity_pass = begin_ax_identity_pass();
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    let show_hidden_app_windows = CONFIG.read().unwrap().windows.show_hidden_app_windows;
    let show_other_desktops = CONFIG.read().unwrap().windows.show_other_desktops;
    crate::e2e_state::set_space_membership_source(false);
    // Enumerate all CG windows. SkyLight membership, not transient onscreen state, decides which
    // Space's switchable windows are candidates; AX remains authoritative for window identity.
    let cg_option = K_C_G_WINDOW_LIST_OPTION_ALL;
    let array = unsafe { CGWindowListCopyWindowInfo(cg_option, 0) };
    if array.is_null() {
        return vec![];
    }

    // Own-PID windows are no longer excluded by PID: the settings window is own-PID too, and
    // excluding it would make it unswitchable while open. The overlay itself needs no PID
    // exclusion -- it uses a non-zero overlay level and no AXMain/fullscreen semantics, so the
    // admission gate below drops it. The settings window, when
    // closed, is orderOut'd and excluded by the own-PID onscreen check
    // below, so "open -> shown as a card, closed -> hidden" still holds.
    let mut windows: Vec<WindowInfo> = Vec::new();
    // Windows already shown by the CG loop: skipped by the AX backfill (no duplicate rows).
    let mut shown: HashSet<(i32, u32)> = HashSet::new();
    // TIMING-DEBUG Phase timings (debug tier): locate summon stalls -- CG enumeration /
    // per-PID AX queries / the frontmost lookup. Remove together with the [collect] logs.
    let t0 = Instant::now();
    let count = unsafe { CFArrayGetCount(array) };
    let display_bounds = active_display_bounds();
    let now = Instant::now();
    let ancient_base = now.checked_sub(Duration::from_secs(86_400)).unwrap_or(now);
    // One collection uses one activation snapshot so concurrent notifications cannot give
    // windows in the same batch two different time bases.
    let last_activated = LAST_ACTIVATED.lock().unwrap().clone();
    let t_cg_ms = t0.elapsed().as_millis();
    let mut insertion_order: u32 = 0;

    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    // Snapshot every CG window's layer so AX backfill can distinguish orderOut'd windows from
    // app-owned overlays that were intentionally filtered by the normal layer-0 pass.
    let mut cg_window_layers: HashMap<(i32, u32), i32> = HashMap::new();
    let mut cg_window_bounds: HashMap<(i32, u32), (f64, f64, f64, f64)> = HashMap::new();
    // pid -> app name (for the slow-AX log).
    let mut pid_names: HashMap<i32, String> = HashMap::new();
    let mut cg_window_ids = Vec::new();
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }
        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
            continue;
        }
        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        if cgwid != 0 {
            cg_window_layers.insert((owner_pid, cgwid), layer);
            cg_window_bounds.insert(
                (owner_pid, cgwid),
                cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or_default(),
            );
            cg_window_ids.push(cgwid);
        }
        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }
        pid_names.insert(owner_pid, owner_name);
        pids.insert(owner_pid);
    }
    let parent_ids: HashMap<u32, u32> = skylight::window_parent_ids(&cg_window_ids);

    // Use AX window list as primary source (same as macOS App Switcher)
    // pid -> cache identity (bundle id + mtime). Resolved once per pid in the AX phase so the
    // second pass can look up the cache per-window without an NSRunningApplication call each time
    // (wasteful when one app has many windows).
    let mut icon_ids: HashMap<i32, AppIdentity> = HashMap::new();
    // The AX collection phase (parallel): split PIDs into K chunks (K = min(logical cores,
    // PID count), runtime-adaptive to any Apple Silicon variant) queried simultaneously on
    // worker threads -- AX remote messaging is multi-thread-capable and each chunk merges
    // by key into a result identical to the serial version. Wall clock drops from "sum of
    // all PIDs" to "slowest single PID" (apps like WeChat that stall on every AX question
    // used to dominate serial totals; up to 1.5s for one PID in logs).
    let mut ax_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>> = HashMap::new();
    // Windows recovered from the key/main slots (see AxPartial::ax_recovered_wid_to_info).
    let mut ax_recovered_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>> = HashMap::new();
    // Pids whose AX query succeeded but yielded nothing: collected separately, never condemned
    // here.
    let mut ax_empty_pids: HashSet<i32> = HashSet::new();
    let mut hidden_app_pids: HashSet<i32> = HashSet::new();
    // Aggregate AX query failures once per collection so CG fallback remains diagnosable
    // without restoring noisy per-app success logs.
    let mut ax_failed_pids: Vec<i32> = Vec::new();
    // Apps whose AX windows are ALL untitled (e.g. Microsoft To Do, which has a
    // custom title bar and an empty AXTitle). Their titleless windows are real
    // main windows and must not be dropped as popups.
    let mut titleless_pids: HashSet<i32> = HashSet::new();
    let t_ax = Instant::now();
    {
        let pid_list: Vec<i32> = pids.iter().copied().collect();
        // AX is an IPC service implemented by the target apps, not CPU-bound work. Keep a
        // small fixed cap so a large PID set does not overload WindowServer/AX servers and turn
        // the first frame into a synchronized timeout storm.
        const MAX_AX_WORKERS: usize = 4;
        let partials: Vec<AxPartial> = if pid_list.is_empty() {
            Vec::new()
        } else {
            let workers = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(pid_list.len())
                .clamp(1, MAX_AX_WORKERS);
            let chunk_size = pid_list.len().div_ceil(workers);
            std::thread::scope(|scope| {
                let handles: Vec<_> = pid_list
                    .chunks(chunk_size)
                    .map(|chunk| {
                        scope.spawn(|| {
                            // Safety net: wrap the whole chunk too (covering non-AX ObjC calls such
                            // as identity resolution); on an exception the chunk degrades to "no
                            // data" instead of terminating the process.
                            objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
                                ax_collect_chunk(chunk, &pid_names, show_hidden_app_windows)
                            }))
                            .unwrap_or_else(|exception| {
                                log_info!("[collect] ax exception (chunk) {:?}", exception);
                                ax_partial_after_chunk_exception(chunk)
                            })
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("ax collect worker panicked"))
                    .collect()
            })
        };
        // Merge chunks by key; ax_wid_to_info keys are disjoint (each PID lives in exactly
        // one chunk), so merge order cannot affect the outcome.
        for p in partials {
            icon_ids.extend(p.icon_ids);
            hidden_app_pids.extend(p.hidden_app_pids);
            ax_empty_pids.extend(p.ax_empty_pids);
            ax_recovered_wid_to_info.extend(p.ax_recovered_wid_to_info);
            ax_failed_pids.extend(p.ax_failed_pids);
            ax_wid_to_info.extend(p.ax_wid_to_info);
            titleless_pids.extend(p.titleless_pids);
        }
    }
    ax_failed_pids.sort_unstable();
    ax_failed_pids.dedup();
    if !ax_failed_pids.is_empty() {
        let failed = ax_failed_pids
            .iter()
            .map(|pid| {
                format!(
                    "{}:\"{}\"",
                    pid,
                    pid_names.get(pid).map(String::as_str).unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        log_debug!(
            "[collect] ax fallback: failed_pids={} [{}]",
            ax_failed_pids.len(),
            failed
        );
    }
    // TIMING-DEBUG Wall clock of the parallel AX phase (not the sum of per-PID work;
    // slow queries are logged individually).
    let ax_total_ms = t_ax.elapsed().as_millis();
    if !show_hidden_app_windows && !hidden_app_pids.is_empty() {
        cg_window_layers.retain(|(pid, _), _| !hidden_app_pids.contains(pid));
        cg_window_bounds.retain(|(pid, _), _| !hidden_app_pids.contains(pid));
    }
    // Recovery is discovery only. Keep its original activatable-process filter; the CG pairing
    // below applies the existing placement, size, and title rules before SkyLight membership.
    let pool: *mut AnyObject = unsafe { msg_send![class!(NSAutoreleasePool), new] };
    ax_recovered_wid_to_info.retain(|pid, _| unsafe { !process_cannot_be_activated(*pid) });
    let _: () = unsafe { msg_send![pool, drain] };

    let mut membership_window_ids = cg_window_ids.clone();
    membership_window_ids.extend(
        ax_wid_to_info
            .values()
            .chain(ax_recovered_wid_to_info.values())
            .flat_map(|windows| windows.keys().copied()),
    );
    membership_window_ids.retain(|window_id| *window_id != 0);
    membership_window_ids.sort_unstable();
    membership_window_ids.dedup();
    let (membership_source, membership_snapshot) =
        query_space_membership(&membership_window_ids, true);

    let fullscreen_cg_window_ids = native_fullscreen_cg_window_ids(
        &cg_window_bounds,
        &ax_wid_to_info,
        &ax_recovered_wid_to_info,
        &display_bounds,
    );
    // Window visibility comes from per-display Space membership, not a global fullscreen flag.
    let mut onscreen_ids = HashSet::new();
    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null()
            || cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999) != 0
            || cf_dict_get_bool(dict, "kCGWindowIsOnscreen") != Some(true)
            || cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0) <= 0.0
        {
            continue;
        }
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        if cgwid != 0 && !is_attached_surface(parent_ids.get(&cgwid).copied()) {
            onscreen_ids.insert(cgwid);
        }
    }
    let fullscreen_onscreen_ids = fullscreen_cg_window_ids
        .iter()
        .filter_map(|(_, cgwid)| onscreen_ids.contains(cgwid).then_some(*cgwid))
        .collect();
    if crate::space_transition::observe_window_snapshot(
        onscreen_ids,
        fullscreen_onscreen_ids,
        collection_started_at,
    ) {
        crate::window_refresh::note_space_transition(false);
    }
    remember_non_normal_cg_windows(
        &cg_window_layers,
        &icon_ids,
        &ax_wid_to_info,
        &fullscreen_cg_window_ids,
        &parent_ids,
    );

    // Which apps' `kAXWindows` answered with at least one window, and which apps' key/main slots
    // named one: the A2 snapshot needs both to tell "another desktop's real window" from "a surface
    // AX refuses to name". Staged, published only with an accepted result.
    crate::e2e_state::stage_ax_pid_evidence(
        ax_wid_to_info.keys().copied(),
        ax_recovered_wid_to_info.keys().copied(),
        ax_failed_pids.iter().copied(),
    );

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
            continue;
        }
        // Scope evidence is staged before every admission filter below (the alpha check, the hidden
        // app, the attached-surface rule, the AX pairing, the switch itself): it says where the
        // window lives, not whether it is a card, and the capture pipeline needs it precisely for the
        // windows that are not cards. `Unknown` is not staged, so publishing leaves the previous
        // observation alone.
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        let staged_scope = classify_window_space(
            membership_source,
            membership_snapshot.as_ref(),
            WindowPairingSource::PublishedAx,
            cgwid,
            cf_dict_get_bool(dict, "kCGWindowIsOnscreen"),
        );
        if staged_scope != MembershipScope::Unknown {
            crate::thumbnail::stage_window_scope(
                owner_pid,
                cgwid,
                staged_scope == MembershipScope::OtherDesktop,
            );
        }
        // Fully transparent windows (alpha=0) are invisible; Mission Control doesn't show them.
        let alpha = cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0);
        if alpha <= 0.0 {
            continue;
        }
        if !show_hidden_app_windows && hidden_app_pids.contains(&owner_pid) {
            continue;
        }

        let owner_name = cf_dict_get_string(dict, "kCGWindowOwnerName").unwrap_or_default();
        if owner_name.is_empty() || owner_name == "Dock" {
            continue;
        }

        let cg_title = cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default();
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        // bounds (x, y, w, h); all zeros on parse failure, caller falls back to the main screen.
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));

        // Pair the AX title by CGWindowID (no more order/string guessing).
        // AX is authoritative: a CG window is kept only if AX has a window with
        // the same CGWindowID.
        // Pairing: the kAXWindows answer wins; key/main recovery is discovery only and is
        // published through the same normal-window filters and exact membership gate.
        // Read before pairing: the CG-only fallback for a failed AX read covers only a window we
        // can see, and `classify_window_space` below needs the same fact.
        let cg_is_onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen");
        let published_map = ax_wid_to_info.get(&owner_pid);
        let recovered_map = ax_recovered_wid_to_info.get(&owner_pid);
        let owner_process_start_time_us = icon_ids
            .get(&owner_pid)
            .and_then(|identity| identity.process_start_time_us);
        let (ax_info, pairing_source) = match published_map.and_then(|wid_map| wid_map.get(&cgwid))
        {
            Some(info) => (Some(info), WindowPairingSource::PublishedAx),
            None => match recovered_map
                .and_then(|wid_map| wid_map.get(&cgwid))
                .filter(|_| recovered_window_passes_size_filter(bounds))
            {
                Some(info) => (Some(info), WindowPairingSource::RecoveredAx),
                // AX named other windows of this app but not this one, and has never named this
                // one: a secondary surface AX excludes, not a window of another desktop. Checked
                // before the exception below, because an app whose list is Space-filtered while its
                // key/main slots still answer lands in `ax_empty_pids` and would take that arm
                // first. See `ax_evidence_excludes_this_window`.
                None if ax_evidence_excludes_this_window(
                    published_map.is_some(),
                    recovered_map.map_or(0, HashMap::len),
                    is_ax_identified_window(owner_pid, owner_process_start_time_us, cgwid),
                ) =>
                {
                    continue;
                }
                // AX answered for this app (with a window list, or with nothing at all) and this
                // CG window is in neither. Normally that means an app overlay, menu bar or hidden
                // surface -- EXCEPT when Space membership places the window on another desktop:
                // `kAXWindows` is filtered by the current Space, so a real window there is absent
                // from the answer by construction. That exception is what admission for the
                // "other desktops" switch rests on, and it is why the window keeps its CG-derived
                // title and no AX state.
                None => match cg_only_pairing(
                    published_map.is_some(),
                    recovered_map.is_some(),
                    ax_failed_pids.contains(&owner_pid),
                    cg_is_onscreen.unwrap_or(false),
                ) {
                    CgOnlyPairing::OtherDesktop => {
                        if admits_other_desktop_cg_window(
                            membership_source,
                            membership_snapshot.as_ref(),
                            cgwid,
                            show_other_desktops,
                        ) && cg_only_window_admissible(layer, bounds)
                        {
                            (None, WindowPairingSource::UnpublishedAx)
                        } else {
                            continue;
                        }
                    }
                    CgOnlyPairing::AxUnavailable => (None, WindowPairingSource::AxUnavailable),
                    CgOnlyPairing::Reject => continue,
                },
            },
        };
        // AX identified this window as one of the app's windows: remember it, so a later pass whose
        // Space-filtered answer does not name it (on another desktop) still knows it is not a
        // surface AX refuses to name.
        if matches!(
            pairing_source,
            WindowPairingSource::PublishedAx | WindowPairingSource::RecoveredAx
        ) {
            remember_ax_identity(owner_pid, owner_process_start_time_us, cgwid);
        }

        let (window_title, minimized, is_main, ax_fullscreen, is_custom_root, is_floating_window) =
            ax_info
                .map(|info| {
                    (
                        info.title.clone(),
                        info.minimized,
                        info.is_main,
                        info.is_fullscreen,
                        info.is_custom_root,
                        info.is_floating_window,
                    )
                })
                .unwrap_or((cg_title, false, false, None, false, false));
        let native_fullscreen =
            ax_info.is_some() && native_fullscreen_state(ax_fullscreen, bounds, &display_bounds);

        // The layer rule needs positive AX evidence of fullscreen (see `ax_reports_fullscreen`),
        // and a titled AXFloatingWindow needs the ordinary-window shape as well.
        if !window_admission(layer, is_main, ax_fullscreen, is_floating_window, bounds) {
            continue;
        }
        if is_custom_root && !custom_window_is_substantial(bounds) && !is_main {
            continue;
        }
        if is_known_non_normal_window(
            owner_pid,
            icon_ids
                .get(&owner_pid)
                .and_then(|identity| identity.process_start_time_us),
            cgwid,
        ) && !is_main
        {
            log_debug!(
                "[collect] sticky non-normal layer: pid={} app=\"{}\" cgwid={} -> dropped",
                owner_pid,
                owner_name,
                cgwid
            );
            continue;
        }

        // The enumeration is now All (see the cg_option comment): off-screen windows
        // (orderOut'd dialogs / minimized / other Spaces) are all listed, and whether they
        // show is decided here:
        // - own windows (the settings window): orderOut = closed, filtered by isOnscreen --
        //   open -> shown, closed -> hidden, preserving the original behavior (other apps'
        //   orderOut'd windows can't use this rule; they may be JetBrains-style hidden-but-
        //   legitimate settings dialogs).
        // - AX windows reported after orderOut remain eligible if SkyLight still assigns them to
        //   a current Space; minimized windows follow the setting before any membership check.
        // - every discovered source uses the same membership rule; fullscreen state never
        //   overrides actual Space membership.
        if owner_pid == std::process::id() as i32 && cg_is_onscreen != Some(true) {
            continue;
        }
        let space_scope = classify_window_space(
            membership_source,
            membership_snapshot.as_ref(),
            pairing_source,
            cgwid,
            cg_is_onscreen,
        );
        if !admits_space_scope(
            space_scope,
            WindowSpacePolicy {
                minimized,
                show_minimized,
                show_other_desktops,
            },
        ) {
            crate::e2e_state::space_gate_rejected();
            continue;
        }
        if space_scope == MembershipScope::OtherDesktop {
            crate::e2e_state::other_desktop_accepted();
        }
        if pairing_source == WindowPairingSource::RecoveredAx {
            crate::e2e_state::space_recovered_accepted();
        }

        // Titleless windows are kept only for apps AX confirmed as all-untitled
        // (titleless_pids); the AX-failed fallback no longer exempts empty titles
        // (window identity can't be verified there). A candidate admitted as a window of another
        // desktop is excluded from that exemption: the app-level answer describes the windows AX can
        // see on this desktop and cannot vouch for another desktop's window.
        let titleless_exempt = titleless_exemption_applies(
            space_scope,
            pairing_source,
            titleless_pids.contains(&owner_pid),
        );
        if window_title.is_empty() && !titleless_exempt {
            continue;
        }
        // Shape and title follow the classification, so every route into an "other desktop"
        // candidate (published AX, recovered key/main, AX query failed) is held to the same rule.
        if space_scope == MembershipScope::OtherDesktop
            && !other_desktop_candidate_admissible(layer, bounds, &window_title)
        {
            crate::e2e_state::space_gate_rejected();
            continue;
        }

        // Key mru by (pid, CGWindowID) — CGWindowID is stable for the window's
        // lifetime, more reliable than title (which changes with browser tabs). New windows
        // prefer the app's latest activation as their seed; existing entries are never
        // overwritten, so old same-app windows cannot ride along.
        initialize_window_mru(
            mru,
            owner_pid,
            cgwid,
            last_activated.get(&owner_pid).copied(),
            ancient_base,
            insertion_order,
        );
        insertion_order += 1;
        let identity = icon_ids.get(&owner_pid);
        let icon_path = identity.and_then(check_cache_for_identity);
        // TIMING-DEBUG Flag icon-cache misses: which apps trigger the synchronous extract.
        // Every window-list refresh re-hits the same handful of apps (loginwindow's window
        // never reaches the card list and never gets an icon; an app under development such as
        // PeachPic changes its fingerprint on every rebuild), so log once per (pid, key) per
        // process: the first miss keeps the "who will trigger an extract" signal without
        // flooding the log (loginwindow alone used to print ~600 lines/day). Apps whose
        // extraction is known to fail are skipped by icon_cache entirely -- not even the first
        // line is printed for them.
        if icon_path.is_none()
            && identity.is_some_and(|id| !extraction_known_missing(id, ""))
            && icon_miss_unlogged(owner_pid, identity.map(|id| id.key.as_str()))
        {
            log_debug!(
                "[collect] icon miss: pid={} app=\"{}\"",
                owner_pid,
                owner_name
            );
        }
        windows.push(WindowInfo {
            pid: owner_pid,
            window_id: cgwid,
            app_name: owner_name,
            window_title,
            icon_path,
            is_active: false,
            minimized,
            app_hidden: hidden_app_pids.contains(&owner_pid),
            fullscreen: native_fullscreen,
            on_other_desktop: space_scope == MembershipScope::OtherDesktop,
            bounds,
        });
        shown.insert((owner_pid, cgwid));
    }

    // AX backfill: windows AX reports but the CG enumeration lacks. E.g. JetBrains IDEs
    // orderOut their settings dialog when the main window is activated -- an orderOut'd
    // window is NOT in CGWindowList (the all-Spaces enumeration can still omit orderOut'd
    // windows), so a
    // CG-driven loop can never see it; AX still reports it and it is a legitimate
    // switchable window (BetterCmdTab uses the AX list as its primary source and shows
    // it stably). Entries are built from AX title/minimized; bounds are unknown
    // (off-screen), callers fall back to the main screen.
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        if !show_hidden_app_windows && hidden_app_pids.contains(&pid) {
            continue;
        }
        // Own windows (the settings window) go through the CG isOnscreen filter; not
        // backfilled here: when closed (orderOut) they must not show.
        if pid == std::process::id() as i32 {
            continue;
        }
        let mut entries: Vec<(u32, &AxWindowInfo)> =
            wid_map.iter().map(|(w, info)| (*w, info)).collect();
        entries.sort_by_key(|(w, _)| *w);
        for (cgwid, ax_info) in entries {
            if shown.contains(&(pid, cgwid)) {
                continue;
            }
            if is_attached_surface(parent_ids.get(&cgwid).copied()) {
                continue;
            }
            let process_start_time_us = icon_ids
                .get(&pid)
                .and_then(|identity| identity.process_start_time_us);
            // The sticky classification is checked first: it decides without asking the WindowServer
            // anything, and a window already known to be non-normal is dropped whether or not this
            // pass knows its layer.
            let sticky = is_known_non_normal_window(pid, process_start_time_us, cgwid);
            let layer = if sticky {
                None
            } else {
                ax_only_layer_for_backfill(cg_window_layers.get(&(pid, cgwid)).copied(), || {
                    window_layer_now(cgwid)
                })
            };
            if !backfill_admission(
                layer,
                cg_window_bounds.get(&(pid, cgwid)).copied(),
                sticky,
                ax_info.is_custom_root,
                ax_info.is_main,
                ax_info.is_floating_window,
            ) {
                log_debug!(
                    "[collect] ax-only skipped: pid={} app=\"{}\" cgwid={} layer={:?} sticky={} floating={}",
                    pid,
                    pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                    cgwid,
                    layer,
                    sticky,
                    ax_info.is_floating_window
                );
                continue;
            }
            // Same filters as the CG path: minimized gated by the switch; empty titles
            // (not titleless) are meaningless.
            if ax_info.title.is_empty() && !titleless_pids.contains(&pid) {
                continue;
            }
            let native_fullscreen = ax_info.is_fullscreen == Some(true);
            let backfill_scope = classify_window_space(
                membership_source,
                membership_snapshot.as_ref(),
                WindowPairingSource::PublishedAx,
                cgwid,
                None,
            );
            if !admits_space_scope(
                backfill_scope,
                WindowSpacePolicy {
                    minimized: ax_info.minimized,
                    show_minimized,
                    show_other_desktops,
                },
            ) {
                crate::e2e_state::space_gate_rejected();
                continue;
            }
            if backfill_scope == MembershipScope::OtherDesktop {
                crate::e2e_state::other_desktop_accepted();
            }
            // This path publishes an AX-reported window, so record the identity: a later pass whose
            // Space-filtered answer cannot name it must still know AX identified it.
            remember_ax_identity(
                pid,
                icon_ids
                    .get(&pid)
                    .and_then(|identity| identity.process_start_time_us),
                cgwid,
            );
            // Same rule as the directed backfill (see there): the restored window cannot skip the
            // classification-based title and shape requirements just because AX, not CG, found it.
            let titleless_exempt = titleless_exemption_applies(
                backfill_scope,
                WindowPairingSource::PublishedAx,
                titleless_pids.contains(&pid),
            );
            if ax_info.title.is_empty() && !titleless_exempt {
                continue;
            }
            if backfill_scope == MembershipScope::OtherDesktop
                && !(layer == Some(0)
                    && cg_window_bounds
                        .get(&(pid, cgwid))
                        .copied()
                        .is_some_and(custom_window_is_substantial)
                    && !ax_info.title.is_empty())
            {
                crate::e2e_state::space_gate_rejected();
                continue;
            }
            initialize_window_mru(
                mru,
                pid,
                cgwid,
                last_activated.get(&pid).copied(),
                ancient_base,
                insertion_order,
            );
            insertion_order += 1;
            let icon_path = icon_ids.get(&pid).and_then(check_cache_for_identity);
            log_debug!(
                "[collect] ax-only window restored: pid={} app=\"{}\" cgwid={}",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                cgwid
            );
            windows.push(WindowInfo {
                pid,
                window_id: cgwid,
                app_name: pid_names.get(&pid).cloned().unwrap_or_default(),
                window_title: ax_info.title.clone(),
                icon_path,
                is_active: false,
                minimized: ax_info.minimized,
                app_hidden: hidden_app_pids.contains(&pid),
                fullscreen: native_fullscreen,
                on_other_desktop: backfill_scope == MembershipScope::OtherDesktop,
                bounds: (0.0, 0.0, 0.0, 0.0),
            });
        }
    }

    // Prune MRU to the live window set. The live set is enumerated in All mode (includes
    // minimized/off-screen windows) rather than the display list -- OnScreenOnly can't see
    // minimized windows, so pruning against the display list would wipe their ordering
    // memory. Without pruning, a recycled CGWindowID would make a new window or_insert
    // the dead window's timestamp (inheritance pollution). The live set uses only the most
    // conservative filters (layer 0, or an AX-confirmed main/fullscreen window, + valid pid)
    // -- better to keep than to drop; display
    // filters (AX pairing / Dock / alpha) don't apply here. Cost: one extra All-mode
    // enumeration per summon when show_minimized is off (sub-millisecond).
    let mut live_set: HashSet<(i32, u32)> = {
        let src = if show_minimized {
            array // main query is already All mode
        } else {
            unsafe { CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0) }
        };
        let mut s: HashSet<(i32, u32)> = HashSet::new();
        if !src.is_null() {
            let n = unsafe { CFArrayGetCount(src) };
            for i in 0..n {
                let dict = unsafe { CFArrayGetValueAtIndex(src, i) };
                if dict.is_null() {
                    continue;
                }
                let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);
                let pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
                if pid <= 0 {
                    continue;
                }
                let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
                if is_attached_surface(parent_ids.get(&cgwid).copied()) {
                    continue;
                }
                let is_main_or_fullscreen = ax_wid_to_info
                    .get(&pid)
                    .and_then(|windows| windows.get(&cgwid))
                    .is_some_and(|window| window.is_main || window.is_fullscreen == Some(true))
                    || fullscreen_cg_window_ids.contains(&(pid, cgwid));
                if layer != 0 && !is_main_or_fullscreen {
                    continue;
                }
                s.insert((pid, cgwid));
            }
        }
        if !show_minimized && !src.is_null() {
            unsafe { CFRelease(src) };
        }
        s
    };
    // AX-reported windows missing from the CG enumeration (orderOut'd but legitimate)
    // count as alive too: merge them in, or the MRU prune would wipe their ordering
    // memory (they would re-sort to the tail as "new" windows next summon).
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        let process_start_time_us = icon_ids
            .get(&pid)
            .and_then(|identity| identity.process_start_time_us);
        for &cgwid in wid_map.keys() {
            if should_backfill_ax_window_for_process(
                pid,
                cgwid,
                process_start_time_us,
                cg_window_layers.get(&(pid, cgwid)).copied(),
            ) || is_known_non_normal_window(pid, process_start_time_us, cgwid)
            {
                live_set.insert((pid, cgwid));
            }
        }
    }
    let pruned = prune_mru(mru, &live_set);
    // Pruning is observable (debug tier): logged only when entries were dropped, so a
    // normal summon stays silent.
    if pruned > 0 {
        log_debug!("[windows] pruned {} stale MRU entries", pruned);
    }

    // Temporary trace for Ghostty's hidden-window path: compare CG candidates, both AX window
    // sets, and final cards to locate losses in the system snapshot, AX pairing, or assembly.
    for (&pid, app_name) in pid_names
        .iter()
        .filter(|(_, name)| name.as_str() == "Ghostty")
    {
        if !bump_frontmost {
            continue;
        }
        let mut cg_rows = Vec::new();
        for i in 0..count {
            let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
            if dict.is_null() || cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1) != pid {
                continue;
            }
            let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
            cg_rows.push(format!(
                "id={} layer={} alpha={:.2} onscreen={} title={:?} bounds={:?} parent={:?}",
                cgwid,
                cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999),
                cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0),
                cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false),
                cf_dict_get_string(dict, "kCGWindowName").unwrap_or_default(),
                cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or_default(),
                parent_ids.get(&cgwid).copied()
            ));
        }
        let format_ax_rows = |map: Option<&HashMap<u32, AxWindowInfo>>| {
            let mut rows: Vec<_> = map
                .into_iter()
                .flat_map(|windows| windows.iter())
                .map(|(id, info)| {
                    format!(
                        "id={} title={:?} minimized={} main={} fullscreen={:?} custom_root={} recovered={}",
                        id,
                        info.title,
                        info.minimized,
                        info.is_main,
                        info.is_fullscreen,
                        info.is_custom_root,
                        info.only_via_key_or_main
                    )
                })
                .collect();
            rows.sort();
            rows
        };
        let mut final_rows: Vec<_> = windows
            .iter()
            .filter(|window| window.pid == pid)
            .map(|window| {
                format!(
                    "id={} title={:?} minimized={} app_hidden={} bounds={:?}",
                    window.window_id,
                    window.window_title,
                    window.minimized,
                    window.app_hidden,
                    window.bounds
                )
            })
            .collect();
        final_rows.sort();
        log_debug!(
            "[collect] app trace: pid={} app={:?} bump_frontmost={} show_hidden={} show_minimized={} hidden={} ax_failed={} ax_empty={} cg={:?} ax_published={:?} ax_recovered={:?} cards={:?}",
            pid,
            app_name,
            bump_frontmost,
            show_hidden_app_windows,
            show_minimized,
            hidden_app_pids.contains(&pid),
            ax_failed_pids.contains(&pid),
            ax_empty_pids.contains(&pid),
            cg_rows,
            format_ax_rows(ax_wid_to_info.get(&pid)),
            format_ax_rows(ax_recovered_wid_to_info.get(&pid)),
            final_rows
        );
    }

    unsafe { CFRelease(array) };

    let (front_pid, frontmost, fm_ms): (Option<i32>, Option<(i32, u32)>, u128) = if bump_frontmost {
        // Only summon refreshes read and bump the frontmost window; a transient lifecycle
        // window must not rewrite MRU just because it caused a refresh.
        let t_fm = Instant::now();
        let result = unsafe {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let front_app: *mut AnyObject = msg_send![workspace, frontmostApplication];
            if !front_app.is_null() {
                let front_pid: i32 = msg_send![front_app, processIdentifier];
                (
                    Some(front_pid),
                    focused_window_cgwid(front_pid).map(|cgwid| (front_pid, cgwid)),
                )
            } else {
                (None, None)
            }
        };
        (result.0, result.1, t_fm.elapsed().as_millis())
    } else {
        (None, None, 0)
    };
    if bump_frontmost {
        // The frontmost app's AX focused-window query is timed too (a second wait when leaving a
        // slow-responding app).
        if let Some((pid, _)) = frontmost {
            log_debug!(
                "[collect] frontmost pid={} app=\"{}\" {}ms",
                pid,
                pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                fm_ms
            );
        }
        if let Some((pid, cgwid)) = frontmost {
            mru.insert((pid, cgwid), now);
        } else if let Some((pid, cgwid)) = frontmost_fallback(&windows, front_pid) {
            // Restrict fallback to the system frontmost app so an AX failure cannot bump another
            // app's first CG item as most recent.
            mru.insert((pid, cgwid), now);
        }
    }

    // Pure window-level MRU sort: each window is sorted independently by
    // when it was last activated. LAST_ACTIVATED only seeds a newly discovered window once;
    // existing windows are not updated, preventing browser window B from riding A's coattails
    // when switching from app C to browser window A.
    sort_windows_by_mru(&mut windows, mru, now);

    if let Some(first) = windows.first_mut() {
        first.is_active = true;
    }
    // TIMING-DEBUG Summary: total + per-phase timings (for summon-stall diagnosis).
    let total_ms = t0.elapsed().as_millis();
    log_debug!(
        "[collect] total={}ms cg={}ms ax={}ms frontmost={}ms",
        total_ms,
        t_cg_ms,
        ax_total_ms,
        fm_ms
    );
    windows
}

#[cfg(test)]
mod layer_rule_evidence_tests {
    use super::*;

    #[test]
    fn a_display_sized_floating_window_is_not_a_card() {
        // Observed on Telegram's media viewer: layer 101, `AXFullScreen = false`, bounds equal to the
        // display rect, on the desktop's Space. The bounds fallback still answers "fullscreen" (that
        // answer feeds the card's badge and the space-transition observer), but it must not be the
        // evidence that waives the layer rule, or the viewer becomes a second card.
        let display = (0.0, 0.0, 1470.0, 956.0);
        assert!(native_fullscreen_state(
            Some(false),
            (0.0, 0.0, 1470.0, 956.0),
            &[display]
        ));
        assert!(!ax_reports_fullscreen(Some(false)));
        assert!(!admissible_window_placement(
            101,
            false,
            ax_reports_fullscreen(Some(false))
        ));

        // The exemption keeps working for the cases it is for: a window AX reports as fullscreen, or
        // one that is the app's main window.
        assert!(admissible_window_placement(
            101,
            false,
            ax_reports_fullscreen(Some(true))
        ));
        assert!(admissible_window_placement(
            101,
            true,
            ax_reports_fullscreen(Some(false))
        ));
        // A normal window is admissible whatever AX says about fullscreen.
        assert!(admissible_window_placement(
            0,
            false,
            ax_reports_fullscreen(None)
        ));
    }
}

#[cfg(test)]
mod hidden_app_filter_tests {
    use super::should_filter_hidden_app;

    #[test]
    fn hidden_app_filter_is_independent_and_fails_open_when_state_is_unknown() {
        assert!(!should_filter_hidden_app(true, Some(true)));
        assert!(!should_filter_hidden_app(true, Some(false)));
        assert!(!should_filter_hidden_app(true, None));
        assert!(should_filter_hidden_app(false, Some(true)));
        assert!(!should_filter_hidden_app(false, Some(false)));
        assert!(!should_filter_hidden_app(false, None));
    }
}

#[cfg(test)]
mod tab_group_fold_tests {
    use super::*;

    fn window(cgwid: u32, title: &str, minimized: bool, tabs: Option<&[&str]>) -> AxWindowInfo {
        AxWindowInfo {
            cgwid,
            title: title.to_string(),
            minimized,
            is_main: false,
            is_fullscreen: Some(false),
            is_custom_root: false,
            is_floating_window: false,
            only_via_key_or_main: false,
            tab_group: tabs.map(|titles| TabGroupInfo {
                titles: titles.iter().map(|title| title.to_string()).collect(),
            }),
        }
    }

    #[test]
    fn minimized_tab_group_folds_to_the_selected_tab() {
        // The selected tab window exposes the tab bar; background tabs only report their own title.
        let windows = vec![
            window(
                50,
                "π - oh-my-tab",
                true,
                Some(&["~/PycharmProjects", "π - oh-my-tab"]),
            ),
            window(48, "~/PycharmProjects", true, None),
        ];
        let folded = fold_tab_group(windows);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].cgwid, 50);
    }

    #[test]
    fn non_tab_window_is_kept_while_the_group_folds() {
        // A tab group plus an independent window whose title is not in the tab bar: only the tabs fold.
        let windows = vec![
            window(50, "A", true, Some(&["A", "B"])),
            window(48, "B", true, None),
            window(60, "C", true, None),
        ];
        let folded = fold_tab_group(windows);
        assert_eq!(folded.len(), 2);
        assert!(folded.iter().any(|window| window.cgwid == 50));
        assert!(folded.iter().any(|window| window.cgwid == 60));
        // A non-minimized window is not a background tab -> it stays (this does not affect the other card
        // of the same group).
        let windows = vec![
            window(50, "A", true, Some(&["A", "B"])),
            window(48, "B", false, None),
        ];
        assert_eq!(fold_tab_group(windows).len(), 2);
    }

    #[test]
    fn fold_is_refused_when_more_windows_match_than_there_are_tabs() {
        // All three windows match tab titles, but a group has at most two background tabs -> cannot be
        // decided, so all of them stay.
        let windows = vec![
            window(50, "T", true, Some(&["A", "B", "C"])),
            window(48, "A", true, None),
            window(60, "B", true, None),
            window(70, "C", true, None),
        ];
        assert_eq!(fold_tab_group(windows).len(), 4);
    }

    #[test]
    fn same_title_windows_without_a_tab_bar_are_never_folded() {
        // No tab bar means no group: both real windows must stay.
        let windows = vec![
            window(1, "Downloads", false, None),
            window(2, "Downloads", false, None),
        ];
        assert_eq!(fold_tab_group(windows).len(), 2);
    }

    #[test]
    fn duplicate_tab_titles_still_fold() {
        // Two tabs sharing a title (terminal tabs in the same cwd) still claim a slot each.
        let windows = vec![
            window(7, "~/code", true, Some(&["~/code", "~/code"])),
            window(8, "~/code", true, None),
        ];
        let folded = fold_tab_group(windows);
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].cgwid, 7);
    }
}

#[cfg(test)]
mod space_gate_tests {
    use super::*;

    fn window_policy(minimized: bool, show_minimized: bool) -> WindowSpacePolicy {
        WindowSpacePolicy {
            minimized,
            show_minimized,
            show_other_desktops: false,
        }
    }

    fn window_policy_with_other_desktops(
        minimized: bool,
        show_minimized: bool,
        show_other_desktops: bool,
    ) -> WindowSpacePolicy {
        WindowSpacePolicy {
            minimized,
            show_minimized,
            show_other_desktops,
        }
    }

    /// The Space classification a SkyLight-sourced window gets, without any window-state option.
    fn scope_of(snapshot: &MembershipSnapshot, cgwid: u32) -> MembershipScope {
        classify_window_space(
            MembershipSource::SkyLight,
            Some(snapshot),
            WindowPairingSource::PublishedAx,
            cgwid,
            None,
        )
    }

    fn membership(current: &[u64], windows: &[(u32, &[u64])]) -> MembershipSnapshot {
        let current_space_ids = current.iter().copied().collect();
        let mut all_spaces: std::collections::HashSet<u64> = current.iter().copied().collect();
        for (_, spaces) in windows {
            all_spaces.extend(spaces.iter().copied());
        }
        let spaces: HashMap<u64, crate::space_groups::SpaceKind> = all_spaces
            .into_iter()
            .map(|space| (space, crate::space_groups::SpaceKind::Ordinary))
            .collect();
        let displays = current
            .iter()
            .enumerate()
            .map(|(index, space)| {
                (
                    format!("display-{index}"),
                    crate::space_groups::DisplaySpaces {
                        current: *space,
                        spaces: spaces.clone(),
                    },
                )
            })
            .collect();
        MembershipSnapshot {
            current_space_ids,
            window_space_ids: windows
                .iter()
                .map(|(window_id, spaces)| (*window_id, spaces.to_vec()))
                .collect(),
            topology: crate::space_groups::Topology { displays },
            ..Default::default()
        }
    }

    #[test]
    fn current_space_membership_intersection_controls_visibility() {
        let snapshot = membership(&[10], &[(7, &[10]), (8, &[20]), (9, &[])]);
        assert_eq!(scope_of(&snapshot, 7), MembershipScope::CurrentGroup);
        assert_eq!(scope_of(&snapshot, 8), MembershipScope::OtherDesktop);
        assert_eq!(scope_of(&snapshot, 9), MembershipScope::Unknown);
        assert_eq!(scope_of(&snapshot, 99), MembershipScope::Unknown);
        // The switch only widens admission: the current group and the unknown case are unchanged.
        assert!(passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::PublishedAx,
            7,
            None,
            window_policy_with_other_desktops(false, false, true),
        ));
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::PublishedAx,
            9,
            None,
            window_policy_with_other_desktops(false, false, true),
        ));
    }

    #[test]
    fn a_managed_space_outside_the_group_needs_the_other_desktops_switch() {
        // Space 20 is managed but belongs to no display's current group, so the window lives on
        // another desktop: rejected while the switch is off, admitted once it is on.
        let snapshot = membership(&[10], &[(8, &[20])]);
        assert_eq!(scope_of(&snapshot, 8), MembershipScope::OtherDesktop);
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::UnpublishedAx,
            8,
            Some(false),
            window_policy(false, false),
        ));
        assert!(passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::UnpublishedAx,
            8,
            Some(false),
            window_policy_with_other_desktops(false, false, true),
        ));
    }

    /// The same snapshot with the given Space ids removed from the accepted topology. SkyLight
    /// membership can name a Space this topology does not manage (a stale record from a Space that
    /// is gone, or one from a display that was disconnected).
    fn without_managed_spaces(mut snapshot: MembershipSnapshot, ids: &[u64]) -> MembershipSnapshot {
        for display in snapshot.topology.displays.values_mut() {
            for id in ids {
                display.spaces.remove(id);
            }
        }
        snapshot
    }

    #[test]
    fn a_candidate_without_any_ax_evidence_needs_the_ordinary_window_shape() {
        // With the switch on, a window of another desktop has no AX element, so the AX judgments
        // that normally let a custom-root or titled-floating window through are unavailable. The
        // ordinary shape is required instead, or a small layer-0 helper surface would become a card.
        let ordinary = (100.0, 80.0, 800.0, 600.0);
        let small = (0.0, 0.0, 60.0, 30.0);
        assert!(cg_only_window_admissible(0, ordinary));
        assert!(!cg_only_window_admissible(0, small));
        assert!(!cg_only_window_admissible(3, ordinary));
        assert!(!cg_only_window_admissible(101, ordinary));
    }

    #[test]
    fn the_titleless_exemption_never_covers_a_window_of_another_desktop() {
        // The app-level answer comes from the windows AX can see on this desktop, so it cannot
        // vouch for another desktop's window -- whatever pairing source carried that candidate.
        for source in [
            WindowPairingSource::PublishedAx,
            WindowPairingSource::RecoveredAx,
            WindowPairingSource::AxUnavailable,
            WindowPairingSource::UnpublishedAx,
        ] {
            assert!(
                !titleless_exemption_applies(MembershipScope::OtherDesktop, source, true),
                "{source:?} must not excuse an untitled window of another desktop"
            );
        }
        // The current group keeps the pre-existing exemption.
        assert!(titleless_exemption_applies(
            MembershipScope::CurrentGroup,
            WindowPairingSource::PublishedAx,
            true
        ));
        assert!(titleless_exemption_applies(
            MembershipScope::CurrentGroup,
            WindowPairingSource::RecoveredAx,
            true
        ));
        assert!(!titleless_exemption_applies(
            MembershipScope::Unknown,
            WindowPairingSource::PublishedAx,
            true
        ));
    }

    #[test]
    fn an_other_desktop_candidate_needs_the_ordinary_shape_and_its_own_title() {
        let ordinary = (100.0, 80.0, 800.0, 600.0);
        assert!(other_desktop_candidate_admissible(0, ordinary, "Title"));
        // A helper-shaped surface and an untitled window are both refused, on every route.
        assert!(!other_desktop_candidate_admissible(
            0,
            (0.0, 0.0, 60.0, 30.0),
            "Title"
        ));
        assert!(!other_desktop_candidate_admissible(0, ordinary, ""));
        assert!(!other_desktop_candidate_admissible(3, ordinary, "Title"));
    }

    #[test]
    fn the_switch_never_admits_windows_without_accepted_space_evidence() {
        // A window whose memberships are empty, or name a Space the accepted topology does not
        // manage, has no evidence that it lives on another desktop -- which is also what an
        // orderOut'd or helper surface presents. Neither shape may enter through the switch.
        let snapshot = membership(&[10], &[(7, &[]), (8, &[999]), (9, &[10, 999])]);
        let snapshot = without_managed_spaces(snapshot, &[999]);
        assert_eq!(scope_of(&snapshot, 7), MembershipScope::Unknown);
        assert_eq!(scope_of(&snapshot, 8), MembershipScope::Unknown);
        // 10 is still managed and in the group, so the window keeps its current-group admission
        // and the unmanaged id alongside it changes nothing.
        assert_eq!(scope_of(&snapshot, 9), MembershipScope::CurrentGroup);
        for cgwid in [7, 8] {
            assert!(!passes_space_policy(
                MembershipSource::SkyLight,
                Some(&snapshot),
                WindowPairingSource::UnpublishedAx,
                cgwid,
                Some(false),
                window_policy_with_other_desktops(false, false, true),
            ));
        }
    }

    #[test]
    fn an_attribute_error_separates_no_value_from_a_failed_read() {
        // The app answered that it has no such window / does not support the attribute.
        assert!(ax_error_means_no_answer(K_AX_ERROR_NO_VALUE));
        assert!(ax_error_means_no_answer(K_AX_ERROR_ATTRIBUTE_UNSUPPORTED));
        assert!(ax_error_means_no_answer(K_AX_ERROR_NOT_IMPLEMENTED));
        // The read itself failed, so nothing is known about the app: this keeps the CG fallback.
        assert!(!ax_error_means_no_answer(K_AX_ERROR_CANNOT_COMPLETE));
        assert!(!ax_error_means_no_answer(K_AX_ERROR_INVALID_UI_ELEMENT));
        assert!(!ax_error_means_no_answer(K_AX_ERROR_FAILURE));
        assert!(!ax_error_means_no_answer(0)); // an unrecognised code is treated as failed
    }

    #[test]
    fn an_incomplete_read_is_not_an_empty_answer() {
        // Empty window list, AXFocusedWindow failed, AXMainWindow has no value: incomplete, so the
        // CG fallback must stay.
        assert!(ax_read_is_incomplete(0, false, false, true));
        // Stats: the app answered an empty list and reported NoValue twice -- it answered.
        assert!(!ax_read_is_incomplete(0, false, false, false));
        // A real window came back: nothing is incomplete about it.
        assert!(!ax_read_is_incomplete(3, false, false, false));
        // The key/main slot recovered a window even though the list was empty and a read failed.
        assert!(!ax_read_is_incomplete(0, true, false, true));
    }

    #[test]
    fn a_thrown_chunk_reports_its_pids_as_failed_not_empty() {
        let partial = ax_partial_after_chunk_exception(&[7, 9]);
        assert_eq!(partial.ax_failed_pids, vec![7, 9]);
        assert!(partial.ax_empty_pids.is_empty());
        assert!(partial.ax_wid_to_info.is_empty());
        assert!(partial.ax_recovered_wid_to_info.is_empty());
    }

    #[test]
    fn a_cg_window_ax_did_not_identify_follows_the_apps_ax_state() {
        // Stats while closed: the published list is empty, the key/main slots answered nothing, and
        // the query succeeded -- so its panel is rejected on every desktop, not just across Spaces.
        assert_eq!(
            cg_only_pairing(false, false, false, false),
            CgOnlyPairing::Reject
        );
        assert_eq!(
            cg_only_pairing(false, false, false, true),
            CgOnlyPairing::Reject
        );
        // A single-window app on another desktop (Telegram): the key/main slot names its window.
        assert_eq!(
            cg_only_pairing(false, true, false, false),
            CgOnlyPairing::OtherDesktop
        );
        // An app with a window on the current desktop may legitimately have one on another.
        assert_eq!(
            cg_only_pairing(true, false, false, false),
            CgOnlyPairing::OtherDesktop
        );
        assert_eq!(
            cg_only_pairing(true, true, false, false),
            CgOnlyPairing::OtherDesktop
        );
        // A failed query knows nothing: the CG fallback covers a window we can see...
        assert_eq!(
            cg_only_pairing(false, false, true, true),
            CgOnlyPairing::AxUnavailable
        );
        // ...and nothing else: a closed panel, or anything on another desktop, is not admitted.
        assert_eq!(
            cg_only_pairing(false, false, true, false),
            CgOnlyPairing::Reject
        );
    }

    #[test]
    fn a_window_ax_did_not_identify_is_not_invented_from_the_cg_list() {
        // 微信's shape from another desktop: the published list is Space-filtered and empty, the
        // key/main slots still identify the real window, and the helper window is in neither and was
        // never identified by AX.
        assert!(ax_evidence_excludes_this_window(false, 1, false));
        // A real second window of the app, identified while its desktop was visited: admissible.
        assert!(!ax_evidence_excludes_this_window(false, 1, true));
        // An app AX told us nothing about (no published window, no key/main window): no evidence
        // either way, so the CG-only fallback -- what the switch is for -- applies.
        assert!(!ax_evidence_excludes_this_window(false, 0, false));
        // An app with a window on the current desktop keeps going through the AX-authority path.
        assert!(!ax_evidence_excludes_this_window(true, 1, false));
    }

    #[test]
    fn other_desktop_pairing_exception_is_narrow() {
        let snapshot = membership(&[10], &[(8, &[20]), (9, &[10]), (11, &[999])]);
        let snapshot = without_managed_spaces(snapshot, &[999]);
        // Switch on + SkyLight + managed-but-elsewhere: the one admitted shape.
        assert!(admits_other_desktop_cg_window(
            MembershipSource::SkyLight,
            Some(&snapshot),
            8,
            true
        ));
        // Off is off.
        assert!(!admits_other_desktop_cg_window(
            MembershipSource::SkyLight,
            Some(&snapshot),
            8,
            false
        ));
        // A window the current group already covers keeps today's AX authority: the exception
        // never substitutes for the AX list on the current desktop.
        assert!(!admits_other_desktop_cg_window(
            MembershipSource::SkyLight,
            Some(&snapshot),
            9,
            true
        ));
        // No accepted Space evidence is never evidence.
        assert!(!admits_other_desktop_cg_window(
            MembershipSource::SkyLight,
            Some(&snapshot),
            11,
            true
        ));
        // Without SkyLight membership there is nothing to attribute, so the legacy source cannot
        // reach the exception even with the switch on.
        assert!(!admits_other_desktop_cg_window(
            MembershipSource::Legacy,
            None,
            8,
            true
        ));
    }

    #[test]
    fn membership_unions_current_spaces_across_displays() {
        let snapshot = membership(&[10, 20], &[(7, &[10]), (8, &[20]), (9, &[30])]);
        assert_eq!(scope_of(&snapshot, 7), MembershipScope::CurrentGroup);
        assert_eq!(scope_of(&snapshot, 8), MembershipScope::CurrentGroup);
        assert_eq!(scope_of(&snapshot, 9), MembershipScope::OtherDesktop);
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::PublishedAx,
            9,
            None,
            window_policy(false, false),
        ));
    }

    #[test]
    fn fullscreen_flags_do_not_bypass_space_membership() {
        // Space membership alone decides: there is no fullscreen-shaped input to this policy any
        // more, so a same-group fullscreen window is admitted and an other-desktop one is not.
        let snapshot = membership(&[10], &[(7, &[10]), (8, &[20])]);
        assert_eq!(scope_of(&snapshot, 8), MembershipScope::OtherDesktop);
        assert!(!passes_space_policy(
            MembershipSource::Legacy,
            None,
            WindowPairingSource::RecoveredAx,
            8,
            Some(false),
            window_policy(false, false),
        ));
    }

    #[test]
    fn minimized_visibility_setting_never_bypasses_membership() {
        let snapshot = membership(&[10], &[(7, &[20])]);
        // Unknown scope: rejected whatever the minimized switch says.
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::RecoveredAx,
            99,
            None,
            window_policy(true, false),
        ));
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::RecoveredAx,
            99,
            None,
            window_policy(true, true),
        ));
        // Other-desktop scope: the minimized option still applies first and independently, so the
        // switch cannot smuggle a minimized window past a closed minimized setting.
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::RecoveredAx,
            7,
            None,
            window_policy_with_other_desktops(true, false, true),
        ));
        assert!(passes_space_policy(
            MembershipSource::SkyLight,
            Some(&snapshot),
            WindowPairingSource::RecoveredAx,
            7,
            None,
            window_policy_with_other_desktops(true, true, true),
        ));
        // A minimized window in the current group only follows the minimized setting.
        let on_current_space = membership(&[10], &[(7, &[10])]);
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&on_current_space),
            WindowPairingSource::RecoveredAx,
            7,
            None,
            window_policy(true, false),
        ));
        assert!(passes_space_policy(
            MembershipSource::SkyLight,
            Some(&on_current_space),
            WindowPairingSource::RecoveredAx,
            7,
            None,
            window_policy(true, true),
        ));
    }

    #[test]
    fn recovered_key_main_discovery_is_independent_of_batch_shape() {
        let bounds = (100.0, 80.0, 800.0, 600.0);
        assert!(recovered_window_passes_size_filter(bounds));
        assert!(!recovered_window_passes_size_filter((0.0, 0.0, 20.0, 20.0)));

        let on_other_space = membership(&[10], &[(42, &[20])]);
        assert!(!passes_space_policy(
            MembershipSource::SkyLight,
            Some(&on_other_space),
            WindowPairingSource::RecoveredAx,
            42,
            None,
            window_policy(false, false),
        ));
        let on_current_space = membership(&[10], &[(42, &[10])]);
        assert!(passes_space_policy(
            MembershipSource::SkyLight,
            Some(&on_current_space),
            WindowPairingSource::RecoveredAx,
            42,
            None,
            window_policy(false, false),
        ));
    }

    #[test]
    fn legacy_fallback_requires_current_space_onscreen_evidence() {
        assert_eq!(
            select_membership_source(true, true),
            MembershipSource::Legacy
        );
        assert_eq!(
            select_membership_source(false, false),
            MembershipSource::Legacy
        );
        assert_eq!(
            select_membership_source(false, true),
            MembershipSource::SkyLight
        );

        assert!(!passes_space_policy(
            MembershipSource::Legacy,
            None,
            WindowPairingSource::RecoveredAx,
            42,
            Some(false),
            window_policy(false, false),
        ));
        assert!(passes_space_policy(
            MembershipSource::Legacy,
            None,
            WindowPairingSource::RecoveredAx,
            42,
            Some(true),
            window_policy(false, false),
        ));
        assert!(!passes_space_policy(
            MembershipSource::Legacy,
            None,
            WindowPairingSource::AxUnavailable,
            42,
            None,
            window_policy(false, false),
        ));
        assert!(passes_space_policy(
            MembershipSource::Legacy,
            None,
            WindowPairingSource::AxUnavailable,
            42,
            Some(true),
            window_policy(false, false),
        ));
        // The switch cannot widen the legacy source: without SkyLight membership there is no way
        // to tell another desktop from an orderOut'd window, so it stays onscreen-only.
        assert!(!passes_space_policy(
            MembershipSource::Legacy,
            None,
            WindowPairingSource::UnpublishedAx,
            42,
            Some(false),
            window_policy_with_other_desktops(false, false, true),
        ));
    }

    #[test]
    fn fullscreen_bounds_fallback_still_rejects_frame_maximized_windows() {
        const DISPLAY: (f64, f64, f64, f64) = (0.0, 0.0, 1000.0, 800.0);
        assert!(cg_bounds_identify_native_fullscreen(DISPLAY, &[DISPLAY]));
        let visible_frame_maximized = (0.0, 24.0, 1000.0, 776.0);
        assert!(!cg_bounds_identify_native_fullscreen(
            visible_frame_maximized,
            &[DISPLAY]
        ));
    }

    #[test]
    fn fullscreen_window_ids_include_recovered_ax_metadata() {
        const DISPLAY: (f64, f64, f64, f64) = (0.0, 0.0, 1000.0, 800.0);
        let cg_bounds = HashMap::from([((7, 42), DISPLAY)]);
        let recovered = HashMap::from([(7, HashMap::from([(42, ax_window(42, Some(true)))]))]);
        let published = HashMap::new();
        let fullscreen_ids =
            native_fullscreen_cg_window_ids(&cg_bounds, &published, &recovered, &[DISPLAY]);
        assert!(fullscreen_ids.contains(&(7, 42)));
    }

    fn ax_window(cgwid: u32, is_fullscreen: Option<bool>) -> AxWindowInfo {
        AxWindowInfo {
            cgwid,
            title: "window".to_string(),
            minimized: false,
            is_main: false,
            is_fullscreen,
            is_custom_root: false,
            is_floating_window: false,
            only_via_key_or_main: true,
            tab_group: None,
        }
    }
}
