//! 窗口收集 · collect:per-PID AX 收集(可并行)与结果合并。
//! Parallelizable per-PID AX collection and result merging.

use super::*;

// ========== per-PID AX 收集(可并行)/ per-PID AX collection (parallelizable) ==========

/// 一个工作线程处理一段 PID 后的部分结果集,线程间按 key 合并——合并后与串行版
/// 逐字段一致(卡片顺序由第二遍 CG 数组遍历决定,与收集顺序无关)。
/// One worker thread's partial result set for its PID chunk; merged by key afterwards --
/// the merge equals the serial version field-for-field (card order is decided by the
/// second pass over the CG array and never depends on collection order).
struct AxPartial {
    icon_ids: HashMap<i32, AppIdentity>,
    /// AX 查询“成功但一个标准窗口都没有”的 pid。空结果**不是**否定证据:AX 只能看到
    /// 当前 Space(AppKit 的 kAXWindows 按当前 Space 过滤),原生全屏 Space 激活时,
    /// 所有后台 App 都会返空——所以这类结果不能直接当“这个 App 没有窗口”处理,而要先
    /// 看整批是否处于退化状态(见 ax_batch_looks_degraded)。
    /// Pids whose AX query SUCCEEDED with zero standard windows. An empty result is NOT negative
    /// evidence: AX only sees the CURRENT Space (AppKit filters kAXWindows by it), so every
    /// background app answers empty while a native fullscreen Space is active -- the result must
    /// not be read as "this app has no windows" before checking whether the whole batch is in
    /// the degraded shape (see ax_batch_looks_degraded).
    ax_empty_pids: HashSet<i32>,
    /// 仅由 key/main 槽位恢复出的窗口(不在 kAXWindows 里),按 pid 分开放:它们不做
    /// Space 过滤,能救回“窗口全在别的 Space”的后台 App,但也会交回辅助进程的浮层,
    /// 所以只在整批退化时才并入配对(见 ax_batch_looks_degraded)。
    /// Windows recovered ONLY from the key/main slots (absent from kAXWindows), kept per pid:
    /// they are not Space-filtered and recover background apps whose windows all live on another
    /// Space, but they also hand over helper processes' overlays -- so they join the pairing only
    /// in the degraded batch (see ax_batch_looks_degraded).
    ax_recovered_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>>,
    ax_failed_pids: Vec<i32>,
    ax_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>>,
    titleless_pids: HashSet<i32>,
    /// 本段所有 PID 的 AX 查询工作耗时之和(诊断用;墙钟由调用方测)。
    /// Sum of AX query work time for this chunk (diagnostics; wall clock is measured by the caller).
    ax_work_ms: u128,
}

/// 处理一段 PID:逐个解析应用身份 + 查询该应用的 AX 窗口列表。AX 远程查询支持
/// 多线程(messaging timeout 按 element 隔离);resolve_app_identity 只读
/// NSRunningApplication 属性 + stat。整段包 autoreleasepool 回收 ObjC 临时对象。
/// Process a chunk of PIDs: resolve each app identity + query its AX window list. AX
/// remote messaging works from any thread (messaging timeouts are per-element);
/// resolve_app_identity only reads NSRunningApplication properties + stat. The whole
/// chunk runs inside an autoreleasepool to drain ObjC temporaries.
///
/// # Safety
/// 调用方需保证无并发的 AX/ObjC 环境冲突(与主线程的 AX 使用互不共享元素)。
/// The caller must ensure no conflicting concurrent use of shared AX/ObjC elements with
/// the main thread.
unsafe fn ax_collect_chunk(chunk: &[i32], pid_names: &HashMap<i32, String>) -> AxPartial {
    let mut partial = AxPartial {
        icon_ids: HashMap::new(),
        ax_empty_pids: HashSet::new(),
        ax_recovered_wid_to_info: HashMap::new(),
        ax_failed_pids: Vec::new(),
        ax_wid_to_info: HashMap::new(),
        titleless_pids: HashSet::new(),
        ax_work_ms: 0,
    };
    // AppKit 临时对象(NSRunningApplication 等)随池回收;图标提取后台线程同款先例。
    // AppKit temporaries (NSRunningApplication et al) drain with the pool; same precedent
    // as the background icon-extraction thread.
    let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
    for &pid in chunk {
        let t_pid = Instant::now();
        let identity = unsafe { resolve_app_identity(pid) };
        let process_start_time_us = identity.process_start_time_us;
        partial.icon_ids.insert(pid, identity);
        // ObjC 异常边界(HIServices 的 AX 内部会抛 NSException)。这类异常穿过 Rust 栈时,
        // 线程级 catch_unwind 接不住"外来异常"(`__rust_foreign_exception` → 整个进程 abort,
        // 实测 2026-09-17 22:20 的崩溃正是如此)。接住后把该 pid 当作"无窗口"降级,记下异常
        // 名/原因,采集继续——进程级崩溃降级为某个 app 的卡片缺失。
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
        // 只记录慢 AX 查询;正常查询保持静默,避免每次召唤刷屏。
        // Log only slow AX queries; successful queries stay silent to avoid flooding every summon.
        // TIMING-DEBUG 慢 AX 查询(≥20ms)单独标记:定位卡顿来自哪个应用(如 Ghostty)。
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
                // 按来源拆开:kAXWindows 的答复是权威列表,key/main 槽位恢复出的窗口只能
                // 在确认“AX 看不到当前 Space 之外”时才用(见 ax_batch_looks_degraded)。
                // Split by origin: the kAXWindows answer is the authoritative list, while windows
                // recovered from the key/main slots are usable only once the batch is confirmed to
                // be in that shape (see ax_batch_looks_degraded).
                let (recovered, published): (Vec<AxWindowInfo>, Vec<AxWindowInfo>) = wins
                    .into_iter()
                    .partition(|window| window.only_via_key_or_main);
                if !published.is_empty() {
                    if windows_are_all_untitled(&published) {
                        partial.titleless_pids.insert(pid);
                    }
                    partial.ax_wid_to_info.insert(pid, ax_wid_map(published));
                } else {
                    // kAXWindows 在当前 Space 看不到这个 App 的任何窗口。
                    // kAXWindows cannot see any of this app's windows from the current Space.
                    partial.ax_empty_pids.insert(pid);
                    // 恢复出来的窗口同样适用“AX 能看到的窗口全部无标题”这条豁免:自绘标题栏
                    // 的 App(标题为空)在退化态下只有这些窗口,不豁免它们就会被空标题门
                    // 全部丢掉,整个 App 从列表里消失。
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
            // AX 查询成功但无标准窗口:可能是该 App 真的没有调度中心可见的窗口(如
            // BetterDisplay 的隐形锚点窗口),也可能是 AX 在当前 Space 看不到它们。
            // 两种情形的区分放在整批收完之后(见 ax_batch_looks_degraded)。
            // AX answered with no standard windows: either the app genuinely has none that
            // Mission Control would show (e.g. an invisible anchor window), or AX cannot see
            // them from the current Space. The two are told apart once the whole batch is in
            // (see ax_batch_looks_degraded).
            Some(_) => {
                partial.ax_empty_pids.insert(pid);
            }
            // AX 查询失败(无 AX 数据):保留 CG 回退路径。
            // AX query failed (no AX data): keep the CG fallback path.
            None => partial.ax_failed_pids.push(pid),
        }
    }
    let _: () = msg_send![pool, drain];
    partial
}

/// 只重新发现一个 PID 的窗口，用于 WindowServer 发现“未显示焦点窗口”后的定向刷新。
/// AX 仍是显示权威，CG 只负责提供当前窗口几何信息和候选集合。
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
/// 为前台应用查找可切换且足够进行缩略图捕获的窗口。
///
/// AX remains the authority for membership in the switcher; the CG snapshot only supplies
/// current geometry. A stale or auxiliary focused-window id therefore cannot force a thin
/// helper surface to become the prewarm target.
/// AX 仍是切换器成员资格的权威，CG 快照只提供当前几何信息；过期或辅助窗口的焦点 id
/// 不会再强行成为预热目标。
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
    // 定向(单 App)刷新只认 kAXWindows 的答复:key/main 槽位不做 Space 过滤,恢复出来的
    // 窗口常是辅助进程的浮层/子窗口,不该由这条路径带进列表。如果答复**只有**恢复出来的
    // 窗口,就当作“没答”(None),让上层保留原有卡片而不是把它们清空。
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

    // CGWindowList 里没有的 AX 窗口仍然是合法窗口,例如 orderOut 的设置对话框。
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

    // 定向收集只清理目标 PID 的死亡窗口,不能修剪其他 PID 的 MRU。
    // Directed collection prunes dead windows only for the target PID; it must not prune other PIDs.
    // AX 暂时漏报时，当前 CG 仍存活的已知窗口不能丢掉 MRU 时间；否则它重新出现会像新窗口。
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

/// AX 批量结果里“CG 里确实有像真窗口、但 AX 返空”的 App 达到这个数量,且它们占了本批
/// 有可切换窗口的 App 的一半及以上时,判定为“AX 看不到当前 Space 之外的窗口”。
/// This many apps that DO have a real-looking CG window yet answered with no standard windows,
/// making up at least half of the apps with switchable windows, marks an AX view that cannot see
/// outside the current Space.
const AX_EMPTY_DEGRADED_MIN_PIDS: usize = 3;

/// 从 (pid, layer, bounds) 里挑出“AX 返空、但 CG 里确实有像真窗口”的 pid 集合。
/// 辅助进程(光标浮层/菜单条/锚点)本来就没有 AX 窗口,不能算退化证据(纯函数,单测覆盖)。
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

/// 判定本批 AX 是否处于“看不到当前 Space 之外”的退化形态(纯函数,单测覆盖)。
/// `empty_pids` = AX 返空且 CG 里确实有像真窗口的 App 数(见 `pids_with_real_window`),
/// `windowed_pids` = 本批有窗口的 App 数。
///
/// 两种情形必须分开:日常个别 App 返空要维持“整 App 跳过”,而**原生全屏 Space 激活时
/// 成批的后台 App 都会返空**——只有后者才是退化,也只有退化时才允许用 CG 元数据兜底。
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

/// “AX 能看到的窗口全部无标题”这条空标题豁免的判定(纯函数,单测覆盖)。
/// 要求至少有一个窗口:空集合不能享受豁免(那会让“一个窗口都没看到”变成“无标题窗口”)。
/// The all-untitled exemption test (pure; unit-tested). At least one window is required: an
/// empty set must not qualify, or "saw no windows" would read as "untitled windows".
pub(super) fn windows_are_all_untitled(windows: &[AxWindowInfo]) -> bool {
    !windows.is_empty() && windows.iter().all(|window| window.title.is_empty())
}

/// 一批 AX 窗口信息 → CGWindowID 索引(cgwid == 0 的条目没有可配对的窗口 id,丢弃)。
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

/// 收集窗口快照,可选择是否把当前前台窗口写入 MRU。
/// 生命周期事件触发的刷新只负责更新窗口集合,不能把自身误当成 summon。
/// Collect a window snapshot, optionally recording the current frontmost window in MRU.
/// Lifecycle-triggered refreshes only update the window set and must not act like a summon.
pub(crate) fn collect_windows_with_frontmost_bump(
    mru: &mut MruMap,
    bump_frontmost: bool,
) -> Vec<WindowInfo> {
    let show_minimized = CONFIG.read().unwrap().windows.show_minimized;
    // 始终用 All 枚举(含离屏窗口)。原因:部分应用(如 JetBrains 系 IDE)在"主窗口被
    // 激活"时会把设置对话框 orderOut(隐藏但保留窗口对象)——它 isOnscreen=false,
    // OnScreenOnly 枚举不到,切换器就会"切到主窗口后设置窗口消失"(BetterCmdTab 用
    // All 枚举,无此问题)。离屏窗口是否显示改由 AX 语义决定(见下文的过滤逻辑):
    // AX 仍报的窗口是合法可切换窗口(调度中心/系统 Cmd+Tab 也认),AX 不报的
    // 隐藏辅助窗口会被 subrole/空标题过滤拦掉。
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

    // 不再按 PID 排除本应用(own-PID)窗口:设置窗口也是 own-PID,排除它会导致设置
    // 开着时切不到它。浮窗自己不需要靠 PID 排除--它使用非 0 的 overlay 层级,
    // kCGWindowLayer != 0,且没有 AXMain/全屏语义,会被下面的准入规则挡掉。设置窗口
    // 关着时 orderOut 离屏,由下文的 own-PID isOnscreen 过滤排除,故
    // "开->显示为卡片、关->不显示"仍然成立。
    //
    // Own-PID windows are no longer excluded by PID: the settings window is own-PID too, and
    // excluding it would make it unswitchable while open. The overlay itself needs no PID
    // exclusion -- it uses a non-zero overlay level and no AXMain/fullscreen semantics, so the
    // admission gate below drops it. The settings window, when
    // closed, is orderOut'd (off-screen) and excluded by the own-PID isOnscreen filter
    // below, so "open -> shown as a card, closed -> hidden" still holds.
    let mut windows: Vec<WindowInfo> = Vec::new();
    // CG 循环里已显示的窗口集合:AX 补漏时跳过(避免重复卡片)。
    // Windows already shown by the CG loop: skipped by the AX backfill (no duplicate rows).
    let mut shown: HashSet<(i32, u32)> = HashSet::new();
    // TIMING-DEBUG 阶段计时(debug 档):定位 summon 卡顿——CG 枚举 / 每 PID AX 查询 / frontmost。
    // TIMING-DEBUG Phase timings (debug tier): locate summon stalls -- CG enumeration /
    // per-PID AX queries / the frontmost lookup. Remove together with the [collect] logs.
    let t0 = Instant::now();
    let count = unsafe { CFArrayGetCount(array) };
    let now = Instant::now();
    let ancient_base = now.checked_sub(Duration::from_secs(86_400)).unwrap_or(now);
    // 一次收集使用同一激活快照，避免同批窗口因通知并发到达而得到两套时间基准。
    // One collection uses one activation snapshot so concurrent notifications cannot give
    // windows in the same batch two different time bases.
    let last_activated = LAST_ACTIVATED.lock().unwrap().clone();
    let t_cg_ms = t0.elapsed().as_millis();
    let mut insertion_order: u32 = 0;

    // 第一遍遍历：收集所有 PID，用于批量查询 AX 窗口
    // First pass: collect all PIDs to batch query AX windows
    let mut pids: HashSet<i32> = HashSet::new();
    // Snapshot every CG window's layer so AX backfill can distinguish orderOut'd windows from
    // app-owned overlays that were intentionally filtered by the normal layer-0 pass.
    // 记录所有 CG 窗口的层级,让 AX 补漏区分合法 orderOut 窗口和被 layer-0 遍历主动过滤的应用浮层。
    let mut cg_window_layers: HashMap<(i32, u32), i32> = HashMap::new();
    // TIMING-DEBUG pid -> 应用名(慢 AX 日志用)/ pid -> app name (for the slow-AX log).
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

    // 以 AX 窗口列表为主数据源（macOS App Switcher 的做法）
    // Use AX window list as primary source (same as macOS App Switcher)
    // pid -> 缓存身份(bundle id + mtime)。AX 循环里按 pid 解析一次,供第二遍按窗口查缓存,
    // 避免每个窗口都做一次 NSRunningApplication 查找(一个 App 多窗口时尤其浪费)。
    // pid -> cache identity (bundle id + mtime). Resolved once per pid in the AX phase so the
    // second pass can look up the cache per-window without an NSRunningApplication call each time
    // (wasteful when one app has many windows).
    let mut icon_ids: HashMap<i32, AppIdentity> = HashMap::new();
    // AX 收集阶段(并行):把 PID 分成 K 组(K = min(逻辑核数, PID 数),运行时自适应
    // 任意 M 系列芯片),各组在工作线程同时查询——AX 远程消息支持多线程,每组结果
    // 按 key 合并后与串行版逐字段一致。墙钟时间从"所有 PID 之和"降为"最慢单 PID"
    // (微信这类对 AX 提问反复磨蹭的应用曾是串行总耗时的主导项,实测单 PID 可达 1.5s)。
    // The AX collection phase (parallel): split PIDs into K chunks (K = min(logical cores,
    // PID count), runtime-adaptive to any Apple Silicon variant) queried simultaneously on
    // worker threads -- AX remote messaging is multi-thread-capable and each chunk merges
    // by key into a result identical to the serial version. Wall clock drops from "sum of
    // all PIDs" to "slowest single PID" (apps like WeChat that stall on every AX question
    // used to dominate serial totals; up to 1.5s for one PID in logs).
    let mut ax_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>> = HashMap::new();
    // key/main 槽位恢复出的窗口(见 AxPartial::ax_recovered_wid_to_info)。
    // Windows recovered from the key/main slots (see AxPartial::ax_recovered_wid_to_info).
    let mut ax_recovered_wid_to_info: HashMap<i32, HashMap<u32, AxWindowInfo>> = HashMap::new();
    // AX 查询“成功但为空”的 pid 集合:单独收集,不在这里定生死。
    // Pids whose AX query succeeded but yielded nothing: collected separately, never condemned
    // here.
    let mut ax_empty_pids: HashSet<i32> = HashSet::new();
    // 每轮聚合 AX 查询失败,保留 CG 回退的诊断证据,同时避免逐应用成功日志刷屏。
    // Aggregate AX query failures once per collection so CG fallback remains diagnosable
    // without restoring noisy per-app success logs.
    let mut ax_failed_pids: Vec<i32> = Vec::new();
    // AX 窗口「全部」为空标题的 App（如 Microsoft To Do：自绘标题栏 -> AXTitle 为空）。
    // 这类 App 的空标题窗口是真实主窗口，不能被当作弹出面板丢弃。
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
                            // 兜底:整块采集再包一层异常边界(覆盖身份解析等非 AX 的 ObjC 调用),
                            // 异常时退化成"这一块没有数据"而不是终止进程。
                            // Safety net: wrap the whole chunk too (covering non-AX ObjC calls such
                            // as identity resolution); on an exception the chunk degrades to "no
                            // data" instead of terminating the process.
                            objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
                                ax_collect_chunk(chunk, &pid_names)
                            }))
                            .unwrap_or_else(|exception| {
                                log_info!("[collect] ax exception (chunk) {:?}", exception);
                                AxPartial {
                                    icon_ids: HashMap::new(),
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
        // 按 key 合并各分段;ax_wid_to_info 的键互不相交(每 PID 只属于一段),
        // 合并顺序不影响最终内容。
        // Merge chunks by key; ax_wid_to_info keys are disjoint (each PID lives in exactly
        // one chunk), so merge order cannot affect the outcome.
        for p in partials {
            icon_ids.extend(p.icon_ids);
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
    // TIMING-DEBUG AX 阶段墙钟(并行后不等于各 PID 工作耗时之和;慢查询会单独记录)。
    // TIMING-DEBUG Wall clock of the parallel AX phase (not the sum of per-PID work;
    // slow queries are logged individually).
    let ax_total_ms = t_ax.elapsed().as_millis();
    // AX 退化判定(见 ax_batch_looks_degraded):退化时“查询成功但为空”不再等于“这个 App
    // 没有窗口”,而是“AX 看不到当前 Space 之外”,于是把 key/main 槽位恢复出的窗口并入配对
    // (仅此一条,不做 CG 整批回退)。判据只数 CG 里确实有像真窗口的 App,否则一屋子本来就
    // 没有 AX 窗口的辅助进程会把判定撑爆。
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
        // 恢复窗口只接受可被激活的进程:不可激活的辅助进程(设置面板宿主、光标浮层服务)
        // 按定义没有切换目标,但它们的 key/main 槽位仍会交回面板/浮层,而尺寸门拦不住
        // 740x883 那种大面板。
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
    remember_non_normal_cg_windows(&cg_window_layers, &icon_ids, &ax_wid_to_info, &parent_ids);

    for i in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, i) };
        if dict.is_null() {
            continue;
        }

        let layer = cf_dict_get_i32(dict, "kCGWindowLayer").unwrap_or(999);

        // 全透明窗口(alpha=0)不可见,调度中心不显示,跳过。
        // Fully transparent windows (alpha=0) are invisible; Mission Control doesn't show them.
        let alpha = cf_dict_get_f64(dict, "kCGWindowAlpha").unwrap_or(1.0);
        if alpha <= 0.0 {
            continue;
        }

        let owner_pid = cf_dict_get_i32(dict, "kCGWindowOwnerPID").unwrap_or(-1);
        if owner_pid <= 0 {
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
        // bounds (x, y, w, h);解析失败时全 0,调用方会回退到主屏幕。
        // bounds (x, y, w, h); all zeros on parse failure, caller falls back to the main screen.
        let bounds = cf_dict_get_bounds(dict, "kCGWindowBounds").unwrap_or((0.0, 0.0, 0.0, 0.0));

        // 按 CGWindowID 精确配对 AX 标题（不再按顺序/字符串猜）。
        // AX 为权威数据源：CG 窗口必须能在 AX 里按 CGWindowID 找到才保留。
        // Pair the AX title by CGWindowID (no more order/string guessing).
        // AX is authoritative: a CG window is kept only if AX has a window with
        // the same CGWindowID.
        // 配对取数:AXWindows 的答复优先;退化态(AX 看不到当前 Space 之外)下再把 key/main
        // 槽位恢复出的窗口并进来,但必须过“像真窗口”的尺寸门——这两个槽位不做 Space 过滤,
        // 也会交回辅助进程的浮层/子窗口。
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
            // kAXWindows 认得这个 App,但这个 CG 窗口不在它的列表里 → 菜单栏/弹出层,跳过。
            // kAXWindows knows this app but not this CG window -> menu bar/popup, skip.
            None if published_map.is_some() => continue,
            None => match recovered_map.and_then(|wid_map| wid_map.get(&cgwid)) {
                Some(info) => Some(info),
                // AX 对这个 App 有答复(给了窗口列表,或明确答复为空),但都没有这个 CG 窗口
                // → 它是应用浮层/菜单栏/隐藏表面,跳过。**退化态也一样**:CG 里存在的表面
                // 不能因为“AX 看不到”就整批当成可切换窗口,否则标签页表面、隐藏窗口、
                // 其他 Space 的残影会全部摊成卡片(AX 配对正是干这个的)。
                // AX answered for this app (with a window list, or with nothing at all) and this
                // CG window is in neither -> it is an app overlay, a menu bar or a hidden surface:
                // skip. The degraded shape changes nothing here -- CG surfaces must not all count
                // as windows merely because AX cannot see them, or tab surfaces, hidden windows and
                // other-Space leftovers would all turn into cards (AX pairing is what prevents
                // that).
                None if published_map.is_some() || ax_empty_pids.contains(&owner_pid) => continue,
                // 无 AX 数据(查询失败) → 退回 CG 标题。
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

        // 枚举已改为 All(见 cg_option 注释),离屏窗口(orderOut 的对话框/最小化/其他
        // Space)全部在列表里,显示与否由以下规则决定:
        // - 本应用自己的窗口(设置窗口):orderOut = 关闭,按 isOnscreen 过滤——
        //   开->显示、关->不显示,维持原行为(其他应用的 orderOut 窗口不能用此规则,
        //   它们可能是 JetBrains 那种"隐藏但合法"的设置对话框)。
        // - 其他应用的窗口:AX 配对成功的都显示——orderOut 但 AX 仍报的窗口
        //   (JetBrains 激活主窗口时隐藏的设置对话框)是合法可切换窗口必须显示;
        //   最小化窗口由"显示最小化窗口"开关控制(AX minimized 标记,与 isOnscreen
        //   无关——最小化窗口 isOnscreen=false 但 minimized=true)。
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

        // 空标题窗口仅对「AX 确认过全部窗口无标题」的 App(titleless_pids)保留;
        // AX 查询失败回退的 App 不再享受空标题豁免(无法确认其窗口身份)。
        // Titleless windows are kept only for apps AX confirmed as all-untitled
        // (titleless_pids); the AX-failed fallback no longer exempts empty titles
        // (window identity can't be verified there).
        if window_title.is_empty() && !titleless_pids.contains(&owner_pid) {
            continue;
        }

        // mru 按 (pid, CGWindowID) 索引——CGWindowID 在窗口生命周期内稳定不变，
        // 比 title 更可靠（title 会随浏览器标签页切换而变）。新窗口优先以 App 最近
        // 激活时间初始化；已有条目不覆盖，因此旧同 App 窗口不会搭便车。
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
        // TIMING-DEBUG 图标缓存 miss 标记:排查 summon 卡顿——哪些 app 会触发同步提取。
        // collect 每次窗口列表刷新都会重新命同一批 app(loginwindow 这类窗口进不了卡片
        // 列表、永远不会有图标,PeachPic 这类开发中的 app 每次重编译换指纹),所以按
        // (pid, key) 每个进程只打一行:既保留「谁会触发提取」的首次信号,又不再刷屏
        // (此前 loginwindow 一天能刷 ~600 行)。已知提取失败的 app 由 icon_cache 直接
        // 跳过提取,连首行都省掉。
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
            bounds,
        });
        shown.insert((owner_pid, cgwid));
    }

    // AX 补漏:AX 窗口列表报、但 CG 枚举没有的窗口。例:JetBrains 系 IDE 在“主窗口
    // 被激活”时把设置对话框 orderOut(隐藏但保留窗口对象)——orderOut 的窗口不在
    // CGWindowList 里(optionAll 也只含屏上窗口),按 CG 遍历无法发现它;但 AX 仍
    // 报它,且它是合法可切换窗口(BetterCmdTab 以 AX 列表为主数据源,故稳定显示)。
    // 这里用 AX 的标题/minimized 补出条目;bounds 未知(离屏),调用方回退主屏幕。
    // AX backfill: windows AX reports but the CG enumeration lacks. E.g. JetBrains IDEs
    // orderOut their settings dialog when the main window is activated -- an orderOut'd
    // window is NOT in CGWindowList (optionAll only covers on-screen windows), so a
    // CG-driven loop can never see it; AX still reports it and it is a legitimate
    // switchable window (BetterCmdTab uses the AX list as its primary source and shows
    // it stably). Entries are built from AX title/minimized; bounds are unknown
    // (off-screen), callers fall back to the main screen.
    for (&pid, wid_map) in ax_wid_to_info.iter() {
        // 本应用窗口(设置窗口)走 CG 路径的 isOnscreen 过滤,这里不补:
        // 关闭(orderOut)时不应出现在切换器里。
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
            // 与 CG 路径同款过滤:最小化由开关控制;空标题(非 titleless)无意义。
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
                bounds: (0.0, 0.0, 0.0, 0.0),
            });
        }
    }

    // 修剪 MRU:只保留存活窗口的条目。存活集用 All 模式枚举(含最小化/离屏窗口),
    // 不用显示列表——OnScreenOnly 看不到最小化窗口,按显示列表修剪会清掉它们的
    // 排序记忆。已关闭窗口的残留条目不清的话,系统复用 CGWindowID 时新窗口会
    // or_insert 命中旧时间戳、按旧窗口的时间排序(继承污染)。存活集只做最保守
    // 过滤(layer 0,或 AX 确认的 main/fullscreen 窗口,+ 有效 pid),宁全勿缺;显示层的过滤
    // (AX 配对/Dock/alpha)不适用。
    // 代价:show_minimized 关闭时每次 summon 补一次 All 枚举(亚毫秒级)。
    //
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
            array // 主查询已是 All 模式,直接复用 / main query is already All mode
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
    // AX 仍报但 CG 枚举不到的窗口(orderOut 的合法窗口)也是存活窗口:并入存活集,
    // 否则 MRU 修剪会清掉它们的排序记忆(下次 summon 又按新窗口排到末尾)。
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
    // 修剪数量可观测(debug 档):有残留才打,平时每次 summon 无噪音。
    // Pruning is observable (debug tier): logged only when entries were dropped, so a
    // normal summon stays silent.
    if pruned > 0 {
        log_debug!("[windows] pruned {} stale MRU entries", pruned);
    }

    unsafe { CFRelease(array) };

    let (front_pid, frontmost, fm_ms): (Option<i32>, Option<(i32, u32)>, u128) = if bump_frontmost {
        // 只有召唤刷新才读取并 bump 当前前台窗口;生命周期刷新不能因临时窗口创建而改写 MRU。
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
        // 前台 app 的 AX 聚焦窗口查询也单独计时(从慢响应 app 切出时这里是第二个等待点)。
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
            // 回退严格限制在系统前台 App 内,避免 AX 失败时把其他 App 的 CG 首项误刷为最新。
            // Restrict fallback to the system frontmost app so an AX failure cannot bump another
            // app's first CG item as most recent.
            mru.insert((pid, cgwid), now);
        }
    }

    // 纯窗口级 MRU 排序：每个窗口独立按最后被激活的时间排序。
    // LAST_ACTIVATED 只为新窗口生成一次初始窗口时间；已有窗口不更新，避免从 App C
    // 切到浏览器窗口 A 时，浏览器的旧窗口 B 也搭便车排到 C 前面。
    // Pure window-level MRU sort: each window is sorted independently by
    // when it was last activated. LAST_ACTIVATED only seeds a newly discovered window once;
    // existing windows are not updated, preventing browser window B from riding A's coattails
    // when switching from app C to browser window A.
    sort_windows_by_mru(&mut windows, mru, now);

    if let Some(first) = windows.first_mut() {
        first.is_active = true;
    }
    // TIMING-DEBUG 汇总:总耗时 + 各阶段(排查 summon 卡顿用)。
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
