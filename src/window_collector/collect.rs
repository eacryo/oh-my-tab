//! Parallelizable per-PID AX collection and result merging.

use super::*;

/// One worker thread's partial result set for its PID chunk; merged by key afterwards --
/// the merge equals the serial version field-for-field (card order is decided by the
/// second pass over the CG array and never depends on collection order).
struct AxPartial {
    icon_ids: HashMap<i32, AppIdentity>,
    hidden_app_pids: HashSet<i32>,
    /// Pids whose AX query SUCCEEDED with zero standard windows. An empty result is NOT negative
    /// evidence: AX only sees the CURRENT Space (AppKit filters kAXWindows by it), so every
    /// background app answers empty while a native fullscreen Space is active -- the result must
    /// not be read as "this app has no windows" before checking whether the whole batch is in
    /// the degraded shape (see ax_batch_looks_degraded).
    ax_empty_pids: HashSet<i32>,
    /// Windows recovered ONLY from the key/main slots (absent from kAXWindows), kept per pid:
    /// they are not Space-filtered and recover background apps whose windows all live on another
    /// Space, but they also hand over helper processes' overlays -- so they join the pairing only
    /// in the degraded batch (see ax_batch_looks_degraded).
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
                // Split by origin: the kAXWindows answer is the authoritative list, while windows
                // recovered from the key/main slots are usable only once the batch is confirmed to
                // be in that shape (see ax_batch_looks_degraded).
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
                    // custom title bar (empty titles) has only these in the degraded shape, and
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
            // Mission Control would show (e.g. an invisible anchor window), or AX cannot see
            // them from the current Space. The two are told apart once the whole batch is in
            // (see ax_batch_looks_degraded).
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
/// AX remains authoritative for display; CG only supplies current geometry and candidates.
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
    let windows = collect_windows_for_pid(&mut mru, pid, focused_cgwid.unwrap_or(preferred_cgwid))?;
    choose_switchable_capture_window(&windows, focused_cgwid, preferred_cgwid)
}

unsafe fn collect_windows_for_pid_inner(
    mru: &mut MruMap,
    pid: i32,
    focused_cgwid: u32,
) -> Option<Vec<WindowInfo>> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    let show_hidden_app_windows = CONFIG.read().unwrap().windows.show_hidden_app_windows;
    let app_hidden = unsafe { application_is_hidden(pid) } == Some(true);
    if should_filter_hidden_app(show_hidden_app_windows, app_hidden.then_some(true)) {
        return Some(Vec::new());
    }
    let array = CGWindowListCopyWindowInfo(K_C_G_WINDOW_LIST_OPTION_ALL, 0);
    if array.is_null() {
        return None;
    }

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
    let mut cg_window_layers: HashMap<u32, i32> = HashMap::new();
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
        if cgwid != 0 {
            cg_window_layers.insert(cgwid, layer);
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
        let Some(ax_info) = ax_wid_to_info.get(&cgwid) else {
            continue;
        };
        if !admissible_window_placement(layer, ax_info.is_main, ax_info.is_fullscreen) {
            continue;
        }
        if ax_info.is_custom_root && !custom_window_is_substantial(bounds) && !ax_info.is_main {
            continue;
        }
        if pid == std::process::id() as i32
            && !cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false)
        {
            continue;
        }
        if !show_minimized && ax_info.minimized {
            continue;
        }
        if ax_info.title.is_empty() && !titleless {
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
            window_title: ax_info.title.clone(),
            icon_path: icon_path.clone(),
            is_active: false,
            minimized: ax_info.minimized,
            app_hidden,
            bounds,
        });
        shown.insert(cgwid);
    }
    remember_non_normal_cg_windows_for_process(
        pid,
        identity.process_start_time_us,
        &cg_window_layers,
        &ax_wid_to_info,
        &parent_ids,
    );
    CFRelease(array);

    // AX-only windows remain valid, for example orderOut'd settings dialogs absent from CG.
    if pid != std::process::id() as i32 {
        for (&cgwid, ax_info) in &ax_wid_to_info {
            if shown.contains(&cgwid) || (!show_minimized && ax_info.minimized) {
                continue;
            }
            if is_attached_surface(parent_ids.get(&cgwid).copied()) {
                continue;
            }
            if !should_backfill_ax_window_for_process(
                pid,
                cgwid,
                identity.process_start_time_us,
                cg_window_layers.get(&cgwid).copied(),
            ) {
                continue;
            }
            if ax_info.is_custom_root && !ax_info.is_main {
                continue;
            }
            if ax_info.title.is_empty() && !titleless {
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

/// This many apps that DO have a real-looking CG window yet answered with no standard windows,
/// making up at least half of the apps with switchable windows, marks an AX view that cannot see
/// outside the current Space.
const AX_EMPTY_DEGRADED_MIN_PIDS: usize = 3;

/// Collects the pids that answered empty to AX yet DO have a real-looking CG window. Helper
/// processes (cursor overlays, menu-bar surfaces, anchors) have no AX windows by nature and are
/// not evidence of degradation (pure; unit-tested).
pub(super) fn pids_with_real_window<I>(windows: I, empty_pids: &HashSet<i32>) -> HashSet<i32>
where
    I: IntoIterator<Item = (i32, i32, (f64, f64, f64, f64))>,
{
    windows
        .into_iter()
        .filter(|(pid, layer, bounds)| {
            *layer == 0 && empty_pids.contains(pid) && custom_window_is_substantial(*bounds)
        })
        .map(|(pid, _, _)| pid)
        .collect()
}

/// Decides whether this batch's AX is in the "cannot see outside the current Space" shape (pure;
/// unit-tested). `empty_pids` = apps that answered empty while owning a real-looking CG window
/// (see `pids_with_real_window`), `windowed_pids` = apps that answered with windows.
///
/// The two cases must stay apart: an occasional app answering empty day to day keeps the "skip
/// the app" treatment, whereas **a native fullscreen Space makes background apps answer empty in
/// bulk** -- only the latter is degradation, and only there may the windows the key/main slots
/// recovered be used.
pub(super) fn ax_batch_looks_degraded(empty_pids: usize, windowed_pids: usize) -> bool {
    empty_pids >= AX_EMPTY_DEGRADED_MIN_PIDS && empty_pids * 2 >= empty_pids + windowed_pids
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
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    let show_hidden_app_windows = CONFIG.read().unwrap().windows.show_hidden_app_windows;
    // Always enumerate with All (off-screen windows included). Some apps (JetBrains IDEs)
    // orderOut their settings dialog when the main window is activated -- it then has
    // isOnscreen=false, invisible to OnScreenOnly, so the switcher would lose it after
    // switching to the main window (BetterCmdTab uses All; no such issue). Whether an
    // off-screen window shows is now decided by AX semantics (see the filter below): a
    // window AX still reports is a legitimate switchable window (Mission Control and the
    // system Cmd+Tab agree); hidden helper surfaces AX never reports are dropped by the
    // subrole/empty-title filters.
    let cg_option = K_C_G_WINDOW_LIST_OPTION_ALL;
    let array = unsafe { CGWindowListCopyWindowInfo(cg_option, 0) };
    if array.is_null() {
        return vec![];
    }

    // Own-PID windows are no longer excluded by PID: the settings window is own-PID too, and
    // excluding it would make it unswitchable while open. The overlay itself needs no PID
    // exclusion -- it uses a non-zero overlay level and no AXMain/fullscreen semantics, so the
    // admission gate below drops it. The settings window, when
    // closed, is orderOut'd (off-screen) and excluded by the own-PID isOnscreen filter
    // below, so "open -> shown as a card, closed -> hidden" still holds.
    let mut windows: Vec<WindowInfo> = Vec::new();
    // Windows already shown by the CG loop: skipped by the AX backfill (no duplicate rows).
    let mut shown: HashSet<(i32, u32)> = HashSet::new();
    // TIMING-DEBUG Phase timings (debug tier): locate summon stalls -- CG enumeration /
    // per-PID AX queries / the frontmost lookup. Remove together with the [collect] logs.
    let t0 = Instant::now();
    let count = unsafe { CFArrayGetCount(array) };
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
                                AxPartial {
                                    icon_ids: HashMap::new(),
                                    hidden_app_pids: HashSet::new(),
                                    ax_empty_pids: HashSet::new(),
                                    ax_recovered_wid_to_info: HashMap::new(),
                                    ax_failed_pids: Vec::new(),
                                    ax_wid_to_info: HashMap::new(),
                                    titleless_pids: HashSet::new(),
                                    ax_work_ms: 0,
                                }
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
    // AX degradation (see ax_batch_looks_degraded): in that shape an empty answer no longer means
    // "this app has no windows" but "AX cannot see outside the current Space", so the windows the
    // key/main slots recovered join the pairing (that alone -- no wholesale CG fallback). Only apps
    // that DO have a real-looking CG window count, or a houseful of helper processes that never had
    // AX windows would trip the test.
    let empty_with_real_window = pids_with_real_window(
        (0..count).filter_map(|i| {
            let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
            if dict.is_null() {
                return None;
            }
            Some((
                cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1),
                cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999),
                cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or_default(),
            ))
        }),
        &ax_empty_pids,
    );
    let ax_degraded = ax_batch_looks_degraded(empty_with_real_window.len(), ax_wid_to_info.len());
    if ax_degraded {
        // Recovered windows are accepted only from activatable processes: a non-activatable helper
        // (settings-pane host, cursor-overlay service) has no switch target by definition, yet its
        // key/main slots still hand the pane/overlay over, and a size gate cannot reject a large
        // 740x883 pane.
        let pool: *mut AnyObject = unsafe { msg_send![class!(NSAutoreleasePool), new] };
        ax_recovered_wid_to_info.retain(|pid, _| unsafe { !process_cannot_be_activated(*pid) });
        let _: () = unsafe { msg_send![pool, drain] };
        log_debug!(
            "[collect] ax degraded: empty_real_pids={} windowed_pids={} -> key/main-recovered windows pair",
            empty_with_real_window.len(),
            ax_wid_to_info.len()
        );
    }
    if !show_hidden_app_windows && !hidden_app_pids.is_empty() {
        cg_window_layers.retain(|(pid, _), _| !hidden_app_pids.contains(pid));
    }
    remember_non_normal_cg_windows(&cg_window_layers, &icon_ids, &ax_wid_to_info, &parent_ids);

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);

        // Fully transparent windows (alpha=0) are invisible; Mission Control doesn't show them.
        let alpha = cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0);
        if alpha <= 0.0 {
            continue;
        }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
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
        let cgwid = cf_dict_get_u32(dict, "kCGWindowNumber").unwrap_or(0);
        if is_attached_surface(parent_ids.get(&cgwid).copied()) {
            continue;
        }
        // bounds (x, y, w, h); all zeros on parse failure, caller falls back to the main screen.
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));

        // Pair the AX title by CGWindowID (no more order/string guessing).
        // AX is authoritative: a CG window is kept only if AX has a window with
        // the same CGWindowID.
        // Pairing: the kAXWindows answer wins; in the degraded shape (AX cannot see outside the
        // current Space) windows recovered from the key/main slots join in, but must look like real
        // windows -- those slots are not Space-filtered and also hand over helper overlays.
        let published_map = ax_wid_to_info.get(&owner_pid);
        let recovered_map = if ax_degraded && custom_window_is_substantial(bounds) {
            ax_recovered_wid_to_info.get(&owner_pid)
        } else {
            None
        };
        let ax_info = match published_map.and_then(|wid_map| wid_map.get(&cgwid)) {
            Some(info) => Some(info),
            // kAXWindows knows this app but not this CG window -> menu bar/popup, skip.
            None if published_map.is_some() => continue,
            None => match recovered_map.and_then(|wid_map| wid_map.get(&cgwid)) {
                Some(info) => Some(info),
                // AX answered for this app (with a window list, or with nothing at all) and this
                // CG window is in neither -> it is an app overlay, a menu bar or a hidden surface:
                // skip. The degraded shape changes nothing here -- CG surfaces must not all count
                // as windows merely because AX cannot see them, or tab surfaces, hidden windows and
                // other-Space leftovers would all turn into cards (AX pairing is what prevents
                // that).
                None if published_map.is_some() || ax_empty_pids.contains(&owner_pid) => continue,
                // No AX data (the query failed) -> fall back to the CG title.
                None => None,
            },
        };

        let (window_title, minimized, is_main, is_fullscreen, is_custom_root) = ax_info
            .map(|info| {
                (
                    info.title.clone(),
                    info.minimized,
                    info.is_main,
                    info.is_fullscreen,
                    info.is_custom_root,
                )
            })
            .unwrap_or((cg_title, false, false, false, false));

        if !admissible_window_placement(layer, is_main, is_fullscreen) {
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
        // - other apps' windows: every AX-paired window shows -- an orderOut'd window AX
        //   still reports (JetBrains hiding its settings dialog on main-window activation)
        //   is a legitimate switchable window and must show; minimized windows are gated
        //   by the "show minimized windows" switch (the AX minimized flag -- unrelated to
        //   isOnscreen: a minimized window is isOnscreen=false but minimized=true).
        if owner_pid == std::process::id() as i32 {
            let is_onscreen = cf_dict_get_bool(dict, "kCGWindowIsOnscreen").unwrap_or(false);
            if !is_onscreen {
                continue;
            }
        } else if !show_minimized && minimized {
            continue;
        }

        // Titleless windows are kept only for apps AX confirmed as all-untitled
        // (titleless_pids); the AX-failed fallback no longer exempts empty titles
        // (window identity can't be verified there).
        if window_title.is_empty() && !titleless_pids.contains(&owner_pid) {
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
            bounds,
        });
        shown.insert((owner_pid, cgwid));
    }

    // AX backfill: windows AX reports but the CG enumeration lacks. E.g. JetBrains IDEs
    // orderOut their settings dialog when the main window is activated -- an orderOut'd
    // window is NOT in CGWindowList (optionAll only covers on-screen windows), so a
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
            if !should_backfill_ax_window_for_process(
                pid,
                cgwid,
                process_start_time_us,
                cg_window_layers.get(&(pid, cgwid)).copied(),
            ) {
                let layer = cg_window_layers.get(&(pid, cgwid)).copied();
                log_debug!(
                    "[collect] ax-only skipped non-normal layer: pid={} app=\"{}\" cgwid={} layer={:?}",
                    pid,
                    pid_names.get(&pid).map(String::as_str).unwrap_or("?"),
                    cgwid,
                    layer
                );
                continue;
            }
            // Same filters as the CG path: minimized gated by the switch; empty titles
            // (not titleless) are meaningless.
            if !show_minimized && ax_info.minimized {
                continue;
            }
            if ax_info.is_custom_root && !ax_info.is_main {
                continue;
            }
            if ax_info.title.is_empty() && !titleless_pids.contains(&pid) {
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
                    .is_some_and(|window| window.is_main || window.is_fullscreen);
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
                        "id={} title={:?} minimized={} main={} fullscreen={} custom_root={} recovered={}",
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
            "[collect] app trace: pid={} app={:?} bump_frontmost={} show_hidden={} show_minimized={} hidden={} ax_failed={} ax_empty={} degraded={} cg={:?} ax_published={:?} ax_recovered={:?} cards={:?}",
            pid,
            app_name,
            bump_frontmost,
            show_hidden_app_windows,
            show_minimized,
            hidden_app_pids.contains(&pid),
            ax_failed_pids.contains(&pid),
            ax_empty_pids.contains(&pid),
            ax_degraded,
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
            is_fullscreen: false,
            is_custom_root: false,
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
