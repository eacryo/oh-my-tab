//! 缩略图 · capture:捕获管线(flume 队列 + 单 worker 串行限流)、聚焦预热与几何重试。
//! Capture pipeline (flume queue + single serial-rate-limited worker), focused prewarm, and geometry retries.

use super::*;

// ========== 捕获管线(flume 队列 + 单 worker 串行限流) ==========

/// 捕获优先级。值越大越先执行；同优先级保持首次入队 FIFO。启动预热可被
/// 后续召唤的选中/可见请求原地提升，不需要复制第二份任务。
/// Capture priority. Higher values run first; equal priorities retain initial FIFO
/// order. Startup prewarm work can be promoted in place by later selected/visible
/// requests without duplicating the job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum CapturePriority {
    Startup,
    FocusedPrewarm,
    NewWindow,
    Prefetch,
    Visible,
    Activation,
    Selected,
}

impl CapturePriority {
    fn label(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::NewWindow => "new-window",
            Self::FocusedPrewarm => "focused-prewarm",
            Self::Prefetch => "prefetch",
            Self::Visible => "visible",
            Self::Activation => "activation",
            Self::Selected => "selected",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FocusedPrewarmTarget {
    pub(super) pid: i32,
    pub(super) wid: u32,
    pub(super) pid_generation: u64,
    pub(super) target_revision: u64,
    pub(super) needs_resolution: bool,
    pub(super) consecutive_failures: u8,
    pub(super) next_attempt: Instant,
}

/// The hidden-overlay prewarm has one resident utility thread and one latest target;
/// it never creates a thread per capture and stale PID generations are rejected by the
/// normal capture-state gate.
/// 浮窗关闭时的预热只保留一个常驻 utility 线程和一个最新目标；每次捕获不会新建线程,
/// 旧 PID generation 仍由现有捕获状态门控拒绝。
pub(super) static FOCUSED_PREWARM_TARGET: LazyLock<Mutex<Option<FocusedPrewarmTarget>>> =
    LazyLock::new(|| Mutex::new(None));
static FOCUSED_PREWARM_TARGET_REVISION: AtomicU64 = AtomicU64::new(0);
pub(super) static FOCUSED_PREWARM_WORKER_STARTED: AtomicBool = AtomicBool::new(false);
pub(super) static FOCUSED_PREWARM_EPOCH: AtomicU64 = AtomicU64::new(0);
pub(super) static FOCUSED_PREWARM_ACTIVE_GENERATION: AtomicU64 = AtomicU64::new(0);
static FOCUSED_PREWARM_WAKE: LazyLock<(Mutex<()>, Condvar)> =
    LazyLock::new(|| (Mutex::new(()), Condvar::new()));

fn focused_prewarm_enabled() -> bool {
    crate::config::focused_thumbnail_prewarm_enabled() && crate::theme::thumbnails_enabled()
}

pub(super) fn next_focused_prewarm_target_revision() -> u64 {
    FOCUSED_PREWARM_TARGET_REVISION
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1)
        .max(1)
}

pub(super) fn focused_prewarm_revision_is_current(revision: Option<u64>) -> bool {
    revision
        .is_none_or(|revision| FOCUSED_PREWARM_TARGET_REVISION.load(Ordering::Acquire) == revision)
}

pub(super) fn invalidate_focused_prewarm_target(
    should_clear: impl FnOnce(Option<&FocusedPrewarmTarget>) -> bool,
    reason: &str,
) -> bool {
    let mut target_guard = FOCUSED_PREWARM_TARGET.lock().unwrap();
    if !should_clear(target_guard.as_ref()) {
        return false;
    }
    let cleared = target_guard.take();
    let revision = next_focused_prewarm_target_revision();
    if let Some(target) = cleared {
        log_debug!(
            "[thumb] focused prewarm target invalidated pid={} wid={} revision={} reason={}",
            target.pid,
            target.wid,
            revision,
            reason
        );
    } else {
        log_debug!(
            "[thumb] focused prewarm target invalidated revision={} reason={}",
            revision,
            reason
        );
    }
    true
}

pub(super) fn focused_prewarm_due(
    now: Instant,
    last_capture: Option<Instant>,
    next_attempt: Instant,
) -> bool {
    focused_prewarm_ready_at(last_capture, next_attempt) <= now
}

pub(super) fn focused_prewarm_ready_at(
    last_capture: Option<Instant>,
    next_attempt: Instant,
) -> Instant {
    last_capture
        .map(|captured| captured + Duration::from_millis(FOCUSED_PREWARM_INTERVAL_MS))
        .map_or(next_attempt, |cache_ready| cache_ready.max(next_attempt))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusedPrewarmExitAction {
    Stopped,
    Restart,
}

pub(super) fn focused_prewarm_exit_action(
    worker_epoch: u64,
    current_epoch: u64,
    prewarm_enabled: bool,
    target_present: bool,
) -> FocusedPrewarmExitAction {
    if prewarm_enabled && target_present && worker_epoch != current_epoch {
        // A changed epoch means stop/re-enable raced with cleanup. Panic exits are deliberately
        // fail-stop: a poisoned mutex must not trigger an automatic restart loop.
        // 代际变化表示停止/重新启用与清理发生竞态。panic 则故意安全停机，避免中毒的互斥锁
        // 触发自动重启循环。
        return FocusedPrewarmExitAction::Restart;
    }
    FocusedPrewarmExitAction::Stopped
}

pub(super) fn claim_focused_prewarm_worker(epoch: u64) -> bool {
    if FOCUSED_PREWARM_WORKER_STARTED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let active_generation = epoch.wrapping_add(1).max(1);
    FOCUSED_PREWARM_ACTIVE_GENERATION.store(active_generation, Ordering::Release);
    true
}

pub(super) fn release_focused_prewarm_worker(worker_generation: u64) -> bool {
    let active_generation = worker_generation.wrapping_add(1).max(1);
    if FOCUSED_PREWARM_ACTIVE_GENERATION
        .compare_exchange(active_generation, 0, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // A different generation owns the worker slot; an old worker must not clear it.
        // 新代际已经接管 worker 槽位，旧 worker 不能清掉新代际的状态。
        return false;
    }
    FOCUSED_PREWARM_WORKER_STARTED
        .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

fn finish_focused_prewarm_worker(worker_generation: u64, panicked: bool) {
    if !release_focused_prewarm_worker(worker_generation) {
        return;
    }
    if panicked {
        // Do not touch any mutex after a worker panic: the panic may have poisoned the lock that
        // caused it. A later activation can create a fresh target and explicitly retry.
        // worker panic 后不再访问任何互斥锁：触发 panic 的锁可能已经中毒。后续激活会创建新
        // 目标并显式重试。
        log_info!("[thumb] focused prewarm worker panicked; automatic restart disabled");
        return;
    }
    let action = focused_prewarm_exit_action(
        worker_generation,
        FOCUSED_PREWARM_EPOCH.load(Ordering::Acquire),
        focused_prewarm_enabled(),
        FOCUSED_PREWARM_TARGET.lock().unwrap().is_some(),
    );
    if action == FocusedPrewarmExitAction::Restart {
        start_focused_prewarm_worker();
    }
}

#[derive(Clone, Copy)]
pub(super) struct PendingCapture {
    pub(super) target_px_h: u32,
    pub(super) priority: CapturePriority,
    pub(super) sequence: u64,
    pub(super) token: u64,
    pub(super) pid_generation: u64,
    pub(super) activation_at: Option<Instant>,
    freshness_sequence: u64,
    pub(super) enqueued_at: Instant,
    pub(super) ready_since: Instant,
    pub(super) running: bool,
    pub(super) geometry_retry_not_before: Option<Instant>,
    pub(super) geometry_retry_attempts: u8,
    pub(super) geometry_retry_started_at: Option<Instant>,
    focused_target_revision: Option<u64>,
    /// 外观(明暗主题)切换触发的重拍:允许空白帧覆盖已有帧——旧帧是旧外观像素,
    /// 与其他卡片不一致比暂时空白更刺眼。合并请求时按"或"传播。
    /// Appearance (light/dark) transition recapture: blank frames MAY overwrite the
    /// cached frame -- a stale-appearance frame clashes with every other card worse
    /// than a temporary blank. Merging requests propagates the flag with OR.
    pub(super) appearance_refresh: bool,
    /// 空白重试名额归属于该任务；任务以非入库终止时由 worker 释放。
    /// The task owns a blank-retry slot; the worker releases it when the task terminates
    /// without reaching the normal cache-store path.
    pub(super) blank_retry: bool,
}

#[derive(Clone, Copy)]
pub(super) struct CaptureJob {
    pub(super) key: ThumbKey,
    pub(super) target_px_h: u32,
    pub(super) priority: CapturePriority,
    pub(super) token: u64,
    pub(super) pid_generation: u64,
    pub(super) activation_at: Option<Instant>,
    pub(super) freshness_sequence: u64,
    pub(super) enqueued_at: Instant,
    pub(super) ready_since: Instant,
    pub(super) appearance_refresh: bool,
    pub(super) focused_target_revision: Option<u64>,
    pub(super) blank_retry: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureJobResult {
    Finished,
    BlankRetryScheduled,
    GeometryDeferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ActivationRequestResult {
    Enqueued,
    Merged { running: bool },
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlankRetryEnqueueResult {
    Enqueued,
    Merged,
    Dropped,
}

#[derive(Clone, Copy, Debug, Eq)]
pub(super) struct GeometryRetryDeadline {
    pub(super) deadline: Instant,
    pub(super) sequence: u64,
    pub(super) attempt: u8,
}

impl PartialEq for GeometryRetryDeadline {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.sequence == other.sequence
    }
}

// `attempt` is diagnostic metadata only; equality and ordering identify a wake by
// deadline and sequence. `attempt` 仅用于诊断,不参与 deadline 的身份和排序。

impl Ord for GeometryRetryDeadline {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| self.sequence.cmp(&other.sequence))
    }
}

impl PartialOrd for GeometryRetryDeadline {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GeometryDeferResult {
    Deferred(GeometryRetryDeadline),
    Exhausted,
    Stale,
}

/// 同时记录 queued/in-flight 请求的最高目标、最高优先级和生命周期 token。
/// worker 每次被 channel 信号唤醒后从这里选最高优先级任务，因此 channel 自身
/// 只负责计数/唤醒，不再决定执行顺序。
/// Tracks the highest target, priority, and lifecycle token for queued/in-flight
/// requests. The channel is only a count/wakeup mechanism; on each wake the worker
/// selects the highest-priority job here instead of inheriting channel FIFO order.
#[derive(Default)]
pub(super) struct CaptureState {
    pub(super) desired: HashMap<ThumbKey, PendingCapture>,
    pid_generations: HashMap<i32, u64>,
    terminated_pids: HashSet<i32>,
    next_sequence: u64,
    next_token: u64,
    next_freshness_sequence: u64,
}

impl CaptureState {
    fn next_counter(counter: &mut u64) -> u64 {
        *counter = counter.wrapping_add(1);
        if *counter == 0 {
            *counter = 1;
        }
        *counter
    }

    #[cfg(test)]
    pub(super) fn request(
        &mut self,
        key: ThumbKey,
        target_px_h: u32,
        priority: CapturePriority,
    ) -> bool {
        let pid_generation = self.pid_generations.get(&key.pid).copied().unwrap_or(0);
        self.request_for_generation(key, target_px_h, priority, pid_generation, false)
    }

    pub(super) fn request_for_generation(
        &mut self,
        key: ThumbKey,
        target_px_h: u32,
        priority: CapturePriority,
        pid_generation: u64,
        appearance_refresh: bool,
    ) -> bool {
        if self.terminated_pids.contains(&key.pid)
            || self.pid_generations.get(&key.pid).copied().unwrap_or(0) != pid_generation
        {
            return false;
        }
        let freshness_sequence = Self::next_counter(&mut self.next_freshness_sequence);
        match self.desired.entry(key) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                let sequence = Self::next_counter(&mut self.next_sequence);
                let token = Self::next_counter(&mut self.next_token);
                let now = Instant::now();
                entry.insert(PendingCapture {
                    target_px_h,
                    priority,
                    sequence,
                    token,
                    pid_generation,
                    activation_at: None,
                    freshness_sequence: 0,
                    enqueued_at: now,
                    ready_since: now,
                    running: false,
                    geometry_retry_not_before: None,
                    geometry_retry_attempts: 0,
                    geometry_retry_started_at: None,
                    focused_target_revision: None,
                    appearance_refresh,
                    blank_retry: false,
                });
                true
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let pending = entry.get_mut();
                pending.target_px_h = pending.target_px_h.max(target_px_h);
                pending.priority = pending.priority.max(priority);
                pending.appearance_refresh |= appearance_refresh;
                if priority == CapturePriority::Selected && pending.activation_at.take().is_some() {
                    pending.freshness_sequence = freshness_sequence;
                }
                false
            }
        }
    }

    fn set_focused_target_revision(&mut self, key: ThumbKey, target_revision: u64) {
        if let Some(pending) = self.desired.get_mut(&key) {
            if pending.priority == CapturePriority::FocusedPrewarm {
                pending.focused_target_revision = Some(target_revision);
            }
        }
    }

    pub(super) fn request_activation(
        &mut self,
        key: ThumbKey,
        target_px_h: u32,
        activated_at: Instant,
        pid_generation: u64,
    ) -> ActivationRequestResult {
        if self.terminated_pids.contains(&key.pid)
            || self.pid_generations.get(&key.pid).copied().unwrap_or(0) != pid_generation
        {
            return ActivationRequestResult::Rejected;
        }
        let inserted = self.request_for_generation(
            key,
            target_px_h,
            CapturePriority::Activation,
            pid_generation,
            false,
        );
        if let Some(pending) = self.desired.get_mut(&key) {
            // 同一激活 token 的重复调度(外部激活时 808 路径与 backstop 路径先后
            // 到达)不得推进新鲜度:否则会为正在执行的任务追加一次多余的 follow-up
            // 重拍。真正的新激活(不同 token)照常推进。
            // Duplicate scheduling of the SAME activation token (the 808 path and
            // the backstop path arriving within one external activation) must not
            // advance freshness: it would append a redundant follow-up capture to
            // the running job. A genuinely new activation (different token) still
            // advances it.
            if pending.activation_at != Some(activated_at) {
                let freshness_sequence = Self::next_counter(&mut self.next_freshness_sequence);
                pending.activation_at = Some(activated_at);
                pending.freshness_sequence = freshness_sequence;
            }
        }
        if inserted {
            ActivationRequestResult::Enqueued
        } else {
            ActivationRequestResult::Merged {
                running: self
                    .desired
                    .get(&key)
                    .is_some_and(|pending| pending.running),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn take_next(&mut self) -> Option<CaptureJob> {
        self.take_next_for(false)
    }

    #[cfg(test)]
    pub(super) fn take_next_for(&mut self, interaction_active: bool) -> Option<CaptureJob> {
        self.take_next_for_at(interaction_active, Instant::now())
    }

    fn geometry_retry_ready(pending: &PendingCapture, now: Instant) -> bool {
        pending
            .geometry_retry_not_before
            .is_none_or(|not_before| not_before <= now)
    }

    pub(super) fn take_next_for_at(
        &mut self,
        interaction_active: bool,
        now: Instant,
    ) -> Option<CaptureJob> {
        let key = self
            .desired
            .iter()
            .filter(|(_, pending)| {
                !pending.running
                    && (!interaction_active || pending.priority >= CapturePriority::Visible)
                    && Self::geometry_retry_ready(pending, now)
            })
            .min_by_key(|(_, pending)| (Reverse(pending.priority), pending.sequence))
            .map(|(key, _)| *key)?;
        let pending = self.desired.get_mut(&key)?;
        pending.running = true;
        pending.geometry_retry_not_before = None;
        Some(CaptureJob {
            key,
            target_px_h: pending.target_px_h,
            priority: pending.priority,
            token: pending.token,
            pid_generation: pending.pid_generation,
            activation_at: pending.activation_at,
            freshness_sequence: pending.freshness_sequence,
            enqueued_at: pending.enqueued_at,
            ready_since: pending.ready_since,
            appearance_refresh: pending.appearance_refresh,
            focused_target_revision: pending.focused_target_revision,
            blank_retry: pending.blank_retry,
        })
    }

    pub(super) fn is_current(&self, job: CaptureJob) -> bool {
        self.pid_generations.get(&job.key.pid).copied().unwrap_or(0) == job.pid_generation
            && self
                .desired
                .get(&job.key)
                .is_some_and(|pending| pending.token == job.token)
    }

    pub(super) fn defer_geometry_transition(
        &mut self,
        job: CaptureJob,
        now: Instant,
    ) -> GeometryDeferResult {
        if !self.is_current(job) {
            return GeometryDeferResult::Stale;
        }
        let Some(pending) = self.desired.get_mut(&job.key) else {
            return GeometryDeferResult::Stale;
        };
        if !pending.running {
            return GeometryDeferResult::Stale;
        }
        // 几何过渡期间只恢复同一个有效任务；token/generation 任一失配都不能复活旧任务。
        // Restore only the same live job during a geometry transition; a token or generation
        // mismatch must never resurrect stale or cancelled work.
        let started_at = *pending.geometry_retry_started_at.get_or_insert(now);
        if pending.geometry_retry_attempts >= GEOMETRY_RETRY_MAX_ATTEMPTS
            || now.duration_since(started_at) >= GEOMETRY_RETRY_BUDGET
        {
            self.desired.remove(&job.key);
            return GeometryDeferResult::Exhausted;
        }
        let attempt = pending.geometry_retry_attempts;
        pending.geometry_retry_attempts += 1;
        let deadline = now + geometry_retry_delay(attempt);
        pending.running = false;
        pending.geometry_retry_not_before = Some(deadline);
        pending.ready_since = deadline;
        GeometryDeferResult::Deferred(GeometryRetryDeadline {
            deadline,
            sequence: next_geometry_retry_sequence(),
            attempt: attempt + 1,
        })
    }

    pub(super) fn finish(&mut self, job: CaptureJob) -> bool {
        let Some(pending) = self.desired.get_mut(&job.key) else {
            return false;
        };
        if pending.token != job.token || pending.pid_generation != job.pid_generation {
            return false;
        }
        if pending.target_px_h > job.target_px_h
            || pending.priority > job.priority
            || pending.freshness_sequence > job.freshness_sequence
            // 外观刷新合入正在执行的任务时不改变分辨率/优先级/新鲜度,必须单列,
            // 否则主题重拍会被 finish 静默吞掉,该窗口残留旧主题帧。
            // An appearance refresh merging into a running job changes none of the
            // three fields above, so it needs its own check -- otherwise finish()
            // silently swallows the theme recapture and the window keeps a
            // stale-appearance frame.
            || (pending.appearance_refresh && !job.appearance_refresh)
        {
            pending.running = false;
            pending.ready_since = Instant::now();
            pending.geometry_retry_not_before = None;
            pending.geometry_retry_attempts = 0;
            pending.geometry_retry_started_at = None;
            true
        } else {
            self.desired.remove(&job.key);
            false
        }
    }

    pub(super) fn clear_blank_retry_marker(&mut self, job: CaptureJob) -> bool {
        if !job.blank_retry {
            return false;
        }
        let Some(pending) = self.desired.get_mut(&job.key) else {
            return false;
        };
        if pending.token != job.token || pending.pid_generation != job.pid_generation {
            return false;
        }
        let was_set = pending.blank_retry;
        pending.blank_retry = false;
        was_set
    }

    fn discard_deferred(&mut self, job: CaptureJob) -> bool {
        if !self.is_current(job) {
            return false;
        }
        let Some(pending) = self.desired.get(&job.key) else {
            return false;
        };
        if pending.running || pending.geometry_retry_not_before.is_none() {
            return false;
        }
        self.desired.remove(&job.key);
        true
    }

    pub(super) fn cancel_pid(&mut self, pid: i32) {
        let generation = self.pid_generations.entry(pid).or_default();
        *generation = generation.wrapping_add(1);
        self.terminated_pids.insert(pid);
        self.desired.retain(|key, _| key.pid != pid);
    }

    pub(super) fn invalidate_window(&mut self, key: ThumbKey) -> bool {
        self.desired.remove(&key).is_some()
    }

    pub(super) fn activate_pid(&mut self, pid: i32) {
        let generation = self.pid_generations.entry(pid).or_default();
        *generation = generation.wrapping_add(1);
        self.terminated_pids.remove(&pid);
        self.desired.retain(|key, _| key.pid != pid);
    }

    pub(super) fn cancel_all(&mut self) {
        self.desired.clear();
    }

    pub(super) fn pid_generation(&self, pid: i32) -> u64 {
        self.pid_generations.get(&pid).copied().unwrap_or(0)
    }
}

pub(super) static CAPTURE_STATE: LazyLock<Mutex<CaptureState>> =
    LazyLock::new(|| Mutex::new(CaptureState::default()));
static JOB_TX: OnceLock<flume::Sender<()>> = OnceLock::new();
static THUMB_ENQUEUED: AtomicU64 = AtomicU64::new(0);
static THUMB_INTERACTION_DEFERRED: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_DEFERRED: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_RETRY_EXHAUSTED: AtomicU64 = AtomicU64::new(0);
static THUMB_COMPLETED: AtomicU64 = AtomicU64::new(0);
static THUMB_CAPTURE_FAILED: AtomicU64 = AtomicU64::new(0);
static THUMB_QUEUE_TOTAL_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_QUEUE_MAX_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_QUEUE_SAMPLES: AtomicU64 = AtomicU64::new(0);
static THUMB_CAPTURE_TOTAL_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_CAPTURE_MAX_MS: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_REJECTED: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_TOO_SMALL: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_SOURCE_ASPECT: AtomicU64 = AtomicU64::new(0);
static THUMB_GEOMETRY_EXPECTED_ASPECT: AtomicU64 = AtomicU64::new(0);
static GEOMETRY_RETRY_SCHEDULER: OnceLock<flume::Sender<GeometryRetryDeadline>> = OnceLock::new();
static GEOMETRY_RETRY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const GEOMETRY_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(300);
const GEOMETRY_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
pub(super) const GEOMETRY_RETRY_MAX_ATTEMPTS: u8 = 6;
const GEOMETRY_RETRY_BUDGET: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct GeometryTransitionProbeState {
    space_signature: Vec<u32>,
    unstable_streak: u8,
    stable_streak: u8,
    pub(super) active: bool,
}

#[derive(Clone, Debug, Default)]
pub(super) struct GeometryProbeSnapshot {
    pub(super) active: bool,
    pub(super) abnormal_windows: HashSet<u32>,
    pub(super) abnormal_details: HashMap<u32, GeometryAnomaly>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct GeometryAnomaly {
    pub(super) center_residual: (f64, f64),
    pub(super) center_shift: bool,
    pub(super) transform_changed: bool,
    pub(super) heavily_clipped: bool,
}

/// 将 CGS presentation bounds 的原点归一化到公开 CG window bounds 的坐标空间。
/// CGSGetOnscreenWindowBounds 在当前 macOS 上返回与 kCGWindowBounds 相反的原点，
/// 因此不能直接比较两个矩形的中心；宽高仍直接使用 presentation bounds 的值。
///
/// Normalize the CGS presentation bounds into the public CG window-bounds space.
/// On current macOS, CGSGetOnscreenWindowBounds returns the opposite origin from
/// kCGWindowBounds, so their centers must not be compared directly; presentation
/// width and height are still used as reported.
fn normalized_presentation_center(presentation_bounds: CGRect) -> (f64, f64) {
    (
        -presentation_bounds.x + presentation_bounds.w / 2.0,
        -presentation_bounds.y + presentation_bounds.h / 2.0,
    )
}

/// 返回归一化 presentation center 相对公开 bounds center 的残差。
/// Return the residual between the normalized presentation center and public center.
pub(super) fn normalized_center_residual(
    public_bounds: (f64, f64, f64, f64),
    presentation_bounds: CGRect,
) -> (f64, f64) {
    let public_center = (
        public_bounds.0 + public_bounds.2 / 2.0,
        public_bounds.1 + public_bounds.3 / 2.0,
    );
    let presentation_center = normalized_presentation_center(presentation_bounds);
    (
        presentation_center.0 - public_center.0,
        presentation_center.1 - public_center.1,
    )
}

pub(super) fn center_shift_exceeds(bounds: (f64, f64, f64, f64), residual: (f64, f64)) -> bool {
    residual.0.abs() > (bounds.2 * 0.25).max(80.0) || residual.1.abs() > (bounds.3 * 0.25).max(80.0)
}

pub(super) fn presentation_is_heavily_clipped(
    public_bounds: (f64, f64, f64, f64),
    presentation_bounds: CGRect,
) -> bool {
    let normal_area = public_bounds.2.max(0.0) * public_bounds.3.max(0.0);
    let visible_area = presentation_bounds.w.max(0.0) * presentation_bounds.h.max(0.0);
    normal_area > 0.0 && visible_area / normal_area < 0.25
}

pub(super) fn geometry_anomaly(
    public_bounds: (f64, f64, f64, f64),
    transform: skylight::CGAffineTransform,
    presentation_bounds: CGRect,
) -> Option<GeometryAnomaly> {
    let center_residual = normalized_center_residual(public_bounds, presentation_bounds);
    let center_shift = center_shift_exceeds(public_bounds, center_residual);
    let transform_changed = (transform.a - 1.0).abs()
        + (transform.d - 1.0).abs()
        + transform.b.abs()
        + transform.c.abs()
        > 0.15;
    let heavily_clipped = presentation_is_heavily_clipped(public_bounds, presentation_bounds);
    (center_shift || transform_changed || heavily_clipped).then_some(GeometryAnomaly {
        center_residual,
        center_shift,
        transform_changed,
        heavily_clipped,
    })
}

pub(super) fn geometry_capture_should_defer(
    snapshot: &GeometryProbeSnapshot,
    window_id: u32,
) -> bool {
    // 全局过渡期间保护所有窗口;退出 hysteresis 后只保护本轮仍异常的目标窗口。
    // During a global transition guard every window; after hysteresis exits, guard only
    // the target window still abnormal in this sample.
    snapshot.active || snapshot.abnormal_windows.contains(&window_id)
}

/// 基于当前 Space 的窗口变换判断 WindowServer geometry transition 阶段。
/// The detector is intentionally heuristic: private CGS geometry is sampled together
/// with the public current-Space window set. A Space change clears only the old window
/// set's evidence; the current set may still activate on its own sample.
static GEOMETRY_TRANSITION_PROBE: LazyLock<Mutex<GeometryTransitionProbeState>> =
    LazyLock::new(|| Mutex::new(GeometryTransitionProbeState::default()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GeometryProbeTransition {
    None,
    Activated,
    Deactivated,
}

pub(super) fn update_geometry_probe_state(
    state: &mut GeometryTransitionProbeState,
    signature: &[u32],
    abnormal_count: usize,
) -> GeometryProbeTransition {
    if state.space_signature != signature {
        // Space 变化会清除旧窗口集合的证据;当前新集合若本次异常数达到阈值仍可独立激活。
        // A Space change clears evidence for the old window set; the current new set may
        // still activate independently if this sample reaches the abnormality threshold.
        state.space_signature = signature.to_vec();
        state.unstable_streak = 0;
        state.stable_streak = 0;
        state.active = false;
    }

    if abnormal_count >= 2 {
        state.unstable_streak = state.unstable_streak.saturating_add(1);
        state.stable_streak = 0;
        if !state.active {
            state.active = true;
            return GeometryProbeTransition::Activated;
        }
    } else if state.active {
        state.stable_streak = state.stable_streak.saturating_add(1);
        if state.stable_streak >= 2 {
            state.active = false;
            state.unstable_streak = 0;
            return GeometryProbeTransition::Deactivated;
        }
    } else {
        state.stable_streak = 0;
    }
    GeometryProbeTransition::None
}

/// 在当前 Space 里取最多三个普通窗口，判断它们是否同时发生了明显的异常变换。
/// Sample up to three ordinary windows in the current Space and detect simultaneous
/// abnormal transforms. Two consecutive samples below the abnormal threshold are required
/// to leave the state; a Space change clears only the old set's evidence.
fn window_server_geometry_transition_snapshot() -> GeometryProbeSnapshot {
    let windows = crate::window_collector::ordinary_onscreen_window_bounds();
    let mut signature: Vec<u32> = windows.iter().map(|(wid, _)| *wid).collect();
    signature.sort_unstable();
    let Some(connection) = skylight::cgs_main_connection() else {
        return GeometryProbeSnapshot::default();
    };

    let mut abnormal_windows = HashSet::new();
    let mut abnormal_details = HashMap::new();
    for (window_id, bounds) in &windows {
        let Some((transform, onscreen_bounds)) =
            skylight::cgs_window_presentation_geometry(connection, *window_id)
        else {
            return GeometryProbeSnapshot::default();
        };
        if let Some(anomaly) = geometry_anomaly(*bounds, transform, onscreen_bounds) {
            abnormal_windows.insert(*window_id);
            abnormal_details.insert(*window_id, anomaly);
        }
    }

    let abnormal_count = abnormal_windows.len();
    let mut state = GEOMETRY_TRANSITION_PROBE.lock().unwrap();
    let transition = update_geometry_probe_state(&mut state, &signature, abnormal_count);
    match transition {
        GeometryProbeTransition::Activated => {
            let center_count = abnormal_details
                .values()
                .filter(|detail| detail.center_shift)
                .count();
            let transform_count = abnormal_details
                .values()
                .filter(|detail| detail.transform_changed)
                .count();
            let clipped_count = abnormal_details
                .values()
                .filter(|detail| detail.heavily_clipped)
                .count();
            log_debug!(
                "[geometry-transition] WindowServer geometry transition active abnormal_windows={} center={} transform={} clipped={} sample_streak={}",
                abnormal_count,
                center_count,
                transform_count,
                clipped_count,
                state.unstable_streak
            )
        }
        GeometryProbeTransition::Deactivated => {
            log_debug!("[geometry-transition] WindowServer geometry transition inactive after below-threshold samples")
        }
        GeometryProbeTransition::None => {}
    }
    GeometryProbeSnapshot {
        active: state.active,
        abnormal_windows,
        abnormal_details,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GeometryRejectReason {
    SourceTooSmall,
    SourceAspect,
    ExpectedAspect,
}

impl GeometryRejectReason {
    fn label(self) -> &'static str {
        match self {
            Self::SourceTooSmall => "source-too-small",
            Self::SourceAspect => "source-aspect",
            Self::ExpectedAspect => "expected-aspect",
        }
    }
}

pub(super) fn geometry_reject_reason(
    source_w_px: u32,
    source_h_px: u32,
    expected_bounds: Option<(f64, f64, f64, f64)>,
) -> Option<GeometryRejectReason> {
    if source_w_px < 64 || source_h_px < 64 {
        return Some(GeometryRejectReason::SourceTooSmall);
    }
    let source_aspect = source_w_px as f64 / source_h_px as f64;
    if !(0.08..=12.0).contains(&source_aspect) {
        return Some(GeometryRejectReason::SourceAspect);
    }
    let (_, _, expected_w, expected_h) = expected_bounds?;
    if expected_w <= 0.0 || expected_h <= 0.0 {
        return None;
    }
    let expected_aspect = expected_w / expected_h;
    let aspect_ratio = source_aspect / expected_aspect;
    if (0.25..=4.0).contains(&aspect_ratio) {
        None
    } else {
        Some(GeometryRejectReason::ExpectedAspect)
    }
}

/// 拒绝 WindowServer 在动画中返回的细长/裁剪源帧，并按原因分别统计。
/// Reject thin or clipped source frames and keep independent counters per reason.
fn capture_geometry_reject_reason(
    key: ThumbKey,
    captured: &CapturedWindow,
) -> Option<GeometryRejectReason> {
    let expected_bounds = crate::window_collector::ordinary_onscreen_window_bounds()
        .into_iter()
        .find_map(|(wid, bounds)| (wid == key.wid).then_some(bounds));
    geometry_reject_reason(captured.source_w_px, captured.source_h_px, expected_bounds)
}

fn record_geometry_rejection(reason: GeometryRejectReason) {
    THUMB_GEOMETRY_REJECTED.fetch_add(1, Ordering::Relaxed);
    match reason {
        GeometryRejectReason::SourceTooSmall => {
            THUMB_GEOMETRY_TOO_SMALL.fetch_add(1, Ordering::Relaxed);
        }
        GeometryRejectReason::SourceAspect => {
            THUMB_GEOMETRY_SOURCE_ASPECT.fetch_add(1, Ordering::Relaxed);
        }
        GeometryRejectReason::ExpectedAspect => {
            THUMB_GEOMETRY_EXPECTED_ASPECT.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn update_max(metric: &AtomicU64, value: u64) {
    let mut current = metric.load(Ordering::Relaxed);
    while value > current {
        match metric.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next) => current = next,
        }
    }
}

fn record_thumb_queue_wait(queue_ms: u64) {
    THUMB_QUEUE_SAMPLES.fetch_add(1, Ordering::Relaxed);
    THUMB_QUEUE_TOTAL_MS.fetch_add(queue_ms, Ordering::Relaxed);
    update_max(&THUMB_QUEUE_MAX_MS, queue_ms);
}

fn record_thumb_capture(capture_ms: u64) {
    THUMB_COMPLETED.fetch_add(1, Ordering::Relaxed);
    THUMB_CAPTURE_TOTAL_MS.fetch_add(capture_ms, Ordering::Relaxed);
    update_max(&THUMB_CAPTURE_MAX_MS, capture_ms);
}

fn record_thumb_capture_failed() {
    THUMB_CAPTURE_FAILED.fetch_add(1, Ordering::Relaxed);
}

pub(super) fn geometry_retry_delay(attempt: u8) -> Duration {
    match attempt {
        0 => GEOMETRY_RETRY_INITIAL_DELAY,
        1 => Duration::from_millis(600),
        2 => Duration::from_millis(1_200),
        _ => GEOMETRY_RETRY_MAX_DELAY,
    }
}

fn next_geometry_retry_sequence() -> u64 {
    let mut current = GEOMETRY_RETRY_SEQUENCE.load(Ordering::Relaxed);
    loop {
        let next = current.wrapping_add(1).max(1);
        match GEOMETRY_RETRY_SEQUENCE.compare_exchange_weak(
            current,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return next,
            Err(observed) => current = observed,
        }
    }
}

pub(super) fn run_geometry_retry_scheduler(
    rx: flume::Receiver<GeometryRetryDeadline>,
    job_tx: flume::Sender<()>,
) {
    let mut deadlines = std::collections::BinaryHeap::new();
    loop {
        let Some(next) = deadlines.peek().copied() else {
            match rx.recv() {
                Ok(deadline) => deadlines.push(deadline),
                Err(_) => return,
            }
            continue;
        };

        let wait = next.deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(wait) {
            Ok(deadline) => deadlines.push(deadline),
            Err(flume::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let mut due = false;
                while deadlines
                    .peek()
                    .is_some_and(|deadline| deadline.deadline <= now)
                {
                    deadlines.pop();
                    due = true;
                }
                if due {
                    // 调度器只负责唤醒；是否仍然有效由 CaptureState 的 token、generation
                    // 和 retry_not_before 决定，过期 deadline 的额外 wake 是安全的。
                    // The scheduler only wakes the worker; CaptureState remains authoritative
                    // for token, generation, and retry_not_before, so stale deadlines are safe.
                    // `Full` means a wake is already queued. The worker drains CaptureState
                    // and rereads Instant::now() every round, so coalescing this wake is safe.
                    // `Full` 表示已有 wake;worker 每轮 drain 都重读 Instant,合并唤醒是安全的。
                    match job_tx.try_send(()) {
                        Ok(()) | Err(flume::TrySendError::Full(_)) => {}
                        Err(flume::TrySendError::Disconnected(_)) => return,
                    }
                }
            }
            Err(flume::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn schedule_geometry_retry(deadline: GeometryRetryDeadline, job_tx: flume::Sender<()>) -> bool {
    if let Some(scheduler) = GEOMETRY_RETRY_SCHEDULER.get() {
        return scheduler.send(deadline).is_ok();
    }
    let (scheduler_tx, rx) = flume::unbounded();
    if std::thread::Builder::new()
        .name("thumb-geometry-scheduler".into())
        .spawn(move || run_geometry_retry_scheduler(rx, job_tx))
        .is_err()
    {
        log_debug!(
            "[geometry-transition] failed to start retry scheduler; deferred job will be dropped"
        );
        return false;
    }
    let _ = GEOMETRY_RETRY_SCHEDULER.set(scheduler_tx);
    let Some(scheduler) = GEOMETRY_RETRY_SCHEDULER.get() else {
        log_debug!("[geometry-transition] retry scheduler became unavailable; deferred job will be dropped");
        return false;
    };
    if scheduler.send(deadline).is_err() {
        log_debug!(
            "[geometry-transition] retry scheduler rejected deadline; deferred job will be dropped"
        );
        false
    } else {
        true
    }
}

pub(crate) fn log_capture_metrics(context: &str) {
    let enqueued = THUMB_ENQUEUED.load(Ordering::Relaxed);
    let completed = THUMB_COMPLETED.load(Ordering::Relaxed);
    let queue_total = THUMB_QUEUE_TOTAL_MS.load(Ordering::Relaxed);
    let capture_total = THUMB_CAPTURE_TOTAL_MS.load(Ordering::Relaxed);
    let queue_samples = THUMB_QUEUE_SAMPLES.load(Ordering::Relaxed);
    let avg_queue_ms = queue_total.checked_div(queue_samples.max(1)).unwrap_or(0);
    let avg_capture_ms = capture_total.checked_div(completed.max(1)).unwrap_or(0);
    log_debug!(
        "[perf] thumbnail metrics context={} enqueued={} completed={} failed={} geometry_rejected={} geometry_too_small={} geometry_source_aspect={} geometry_expected_aspect={} queue_samples={} interaction_deferred={} geometry_deferred={} geometry_retry_exhausted={} avg_queue_ms={} max_queue_ms={} avg_capture_ms={} max_capture_ms={}",
        context,
        enqueued,
        completed,
        THUMB_CAPTURE_FAILED.load(Ordering::Relaxed),
        THUMB_GEOMETRY_REJECTED.load(Ordering::Relaxed),
        THUMB_GEOMETRY_TOO_SMALL.load(Ordering::Relaxed),
        THUMB_GEOMETRY_SOURCE_ASPECT.load(Ordering::Relaxed),
        THUMB_GEOMETRY_EXPECTED_ASPECT.load(Ordering::Relaxed),
        queue_samples,
        THUMB_INTERACTION_DEFERRED.load(Ordering::Relaxed),
        THUMB_GEOMETRY_DEFERRED.load(Ordering::Relaxed),
        THUMB_GEOMETRY_RETRY_EXHAUSTED.load(Ordering::Relaxed),
        avg_queue_ms,
        THUMB_QUEUE_MAX_MS.load(Ordering::Relaxed),
        avg_capture_ms,
        THUMB_CAPTURE_MAX_MS.load(Ordering::Relaxed)
    );
}

/// Wake the capture worker after the interaction gate is lifted so deferred background jobs can
/// resume without waiting for another thumbnail request.
pub(crate) fn wake_capture_worker() {
    if let Some(tx) = JOB_TX.get() {
        // 单个 wake 足以让 worker 持续从 CaptureState 取任务；不按 pending 数量复制
        // 唤醒令牌，避免交互结束时一次性灌入大量过期 wake。
        // One wake is enough: the worker drains CaptureState itself. Do not enqueue one token
        // per pending job, which would replay a burst of stale wakes after interaction ends.
        let _ = tx.try_send(());
    }
}

/// 尝试安排一次捕获；返回 false 表示相同窗口已 pending/in-flight，或 worker 已退出。
/// Try to schedule one capture; false means the same window is already pending/in-flight,
/// or the worker has exited.
pub(super) fn enqueue_job(pid: i32, wid: u32, target_px_h: u32, priority: CapturePriority) -> bool {
    enqueue_job_inner(pid, wid, target_px_h, priority, None, false, None)
}

/// 外观(明暗主题)切换的重拍:空白帧允许覆盖已有帧(旧外观像素比暂时空白更刺眼)。
/// Appearance (light/dark) transition recapture: blank frames may overwrite the
/// cached frame (stale-appearance pixels clash harder than a temporary blank).
pub(super) fn enqueue_appearance_job(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    priority: CapturePriority,
) -> bool {
    enqueue_job_inner(pid, wid, target_px_h, priority, None, true, None)
}

/// 仅当 PID 仍处于生产者观察到的 generation 时入队，阻止终止前的延迟任务污染
/// PID 复用后的新进程。
/// Enqueue only while the PID remains in the generation observed by the producer,
/// preventing delayed work from an old process from contaminating a reused PID.
pub(super) fn enqueue_job_for_generation(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    priority: CapturePriority,
    pid_generation: u64,
) -> bool {
    enqueue_job_inner(
        pid,
        wid,
        target_px_h,
        priority,
        Some(pid_generation),
        false,
        None,
    )
}

fn enqueue_focused_prewarm_job(target: &FocusedPrewarmTarget, target_px_h: u32) -> bool {
    if !focused_prewarm_revision_is_current(Some(target.target_revision)) {
        return false;
    }
    enqueue_job_inner(
        target.pid,
        target.wid,
        target_px_h,
        CapturePriority::FocusedPrewarm,
        Some(target.pid_generation),
        false,
        Some(target.target_revision),
    )
}

fn current_frontmost_pid() -> Option<i32> {
    let pid = crate::ffi::frontmost_app_info().1;
    (pid > 0).then_some(pid)
}

/// 记录当前前台 App 的 PID/焦点窗口提示，并启动唯一的预热线程。
/// Record the frontmost PID/focused-window hint and start the single prewarm worker.
pub(crate) fn schedule_focused_prewarm(pid: i32, wid: u32) {
    if !focused_prewarm_enabled() {
        return;
    }

    // 激活回调可能运行在主线程；这里只记录轻量提示，窗口解析和可捕获性校验交给 worker。
    // Activation callbacks may run on the main thread; record only a cheap hint here and let
    // the worker resolve switchability and captureability off the main thread.
    let frontmost_pid = current_frontmost_pid().unwrap_or(pid);
    let target_pid_generation = CAPTURE_STATE.lock().unwrap().pid_generation(frontmost_pid);
    let mut target_guard = FOCUSED_PREWARM_TARGET.lock().unwrap();
    let same_target = target_guard.as_ref().filter(|target| {
        target.pid == frontmost_pid
            && target.wid == wid
            && target.pid_generation == target_pid_generation
    });
    let target_revision = same_target
        .map(|target| target.target_revision)
        .unwrap_or_else(next_focused_prewarm_target_revision);
    let needs_resolution = same_target.is_none_or(|target| target.needs_resolution);
    *target_guard = Some(FocusedPrewarmTarget {
        pid: frontmost_pid,
        wid,
        pid_generation: target_pid_generation,
        target_revision,
        needs_resolution,
        consecutive_failures: 0,
        next_attempt: Instant::now(),
    });
    drop(target_guard);
    start_focused_prewarm_worker();
}

/// 启动后台预热线程(目标由最近一次激活记录);设置开关运行时开启时也可调用。
/// Start the prewarm worker for the most recently recorded target; this is also used when the
/// setting is enabled at runtime.
pub(crate) fn start_focused_prewarm_worker() {
    if !focused_prewarm_enabled() || FOCUSED_PREWARM_TARGET.lock().unwrap().is_none() {
        return;
    }
    let epoch = FOCUSED_PREWARM_EPOCH.load(Ordering::Acquire);
    if !claim_focused_prewarm_worker(epoch) {
        return;
    }
    if std::thread::Builder::new()
        .name("oh-my-tab-thumb-prewarm".into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| run_focused_prewarm(epoch)));
            let panicked = result.is_err();
            finish_focused_prewarm_worker(epoch, panicked);
        })
        .is_err()
    {
        let _ = release_focused_prewarm_worker(epoch);
        FOCUSED_PREWARM_EPOCH.fetch_add(1, Ordering::AcqRel);
        log_debug!("[thumb] failed to start focused prewarm worker");
    }
}

/// Stop promptly and leave the target intact so enabling the setting can restart the worker.
/// 及时停止 worker，但保留目标；重新开启设置后可以无须等待新的激活事件而重启。
pub(crate) fn stop_focused_prewarm_worker() {
    FOCUSED_PREWARM_EPOCH.fetch_add(1, Ordering::AcqRel);
    // Keep the started bit reserved until the worker observes the epoch. If the setting is
    // re-enabled immediately, that same worker adopts the new epoch; two workers cannot overlap.
    // 保留 started 标记直到 worker 观察到 epoch；若立即重新开启，由同一个 worker 接管新 epoch，
    // 从而不会出现两个并发 worker。
    FOCUSED_PREWARM_WAKE.1.notify_all();
}

pub(super) fn focused_prewarm_failure_backoff(consecutive_failures: u8) -> Option<Duration> {
    if consecutive_failures == 0 || consecutive_failures >= FOCUSED_PREWARM_MAX_FAILURES {
        None
    } else {
        Some(Duration::from_millis(
            FOCUSED_PREWARM_INTERVAL_MS
                .saturating_mul(1u64 << consecutive_failures.saturating_sub(1).min(5)),
        ))
    }
}

fn note_focused_prewarm_failure(key: ThumbKey, reason: &str) {
    let mut target_guard = FOCUSED_PREWARM_TARGET.lock().unwrap();
    let Some(target) = target_guard.as_mut() else {
        return;
    };
    if target.pid != key.pid || target.wid != key.wid {
        return;
    }
    target.consecutive_failures = target.consecutive_failures.saturating_add(1);
    // The failure counter is one-based; the third consecutive failure reaches the convergence
    // limit and clears the target immediately.
    // 失败计数从 1 开始；第三次连续失败立即达到收敛上限并清理目标。
    let Some(backoff) = focused_prewarm_failure_backoff(target.consecutive_failures) else {
        let consecutive_failures = target.consecutive_failures;
        drop(target_guard);
        let cleared = invalidate_focused_prewarm_target(
            |target| target.is_some_and(|target| target.pid == key.pid && target.wid == key.wid),
            "failure-limit",
        );
        if cleared {
            log_debug!(
                "[thumb] focused prewarm target cleared after {} consecutive failures pid={} wid={} reason={}",
                consecutive_failures,
                key.pid,
                key.wid,
                reason
            );
        }
        return;
    };
    target.next_attempt = Instant::now() + backoff;
    log_debug!(
        "[thumb] focused prewarm failure pid={} wid={} count={} backoff_ms={} reason={}",
        key.pid,
        key.wid,
        target.consecutive_failures,
        backoff.as_millis(),
        reason
    );
}

fn note_focused_prewarm_success(key: ThumbKey) {
    let mut target_guard = FOCUSED_PREWARM_TARGET.lock().unwrap();
    if let Some(target) = target_guard.as_mut() {
        if target.pid == key.pid && target.wid == key.wid {
            target.consecutive_failures = 0;
            target.next_attempt =
                Instant::now() + Duration::from_millis(FOCUSED_PREWARM_INTERVAL_MS);
        }
    }
}

fn clear_focused_prewarm_if(key: ThumbKey, reason: &str) {
    let _ = invalidate_focused_prewarm_target(
        |target| target.is_some_and(|target| target.pid == key.pid && target.wid == key.wid),
        reason,
    );
}

pub(super) fn update_focused_prewarm_target_if_current(
    expected: &FocusedPrewarmTarget,
    selected_wid: u32,
) -> Option<FocusedPrewarmTarget> {
    let mut target_guard = FOCUSED_PREWARM_TARGET.lock().unwrap();
    let target = target_guard.as_mut()?;
    if target.pid != expected.pid
        || target.wid != expected.wid
        || target.target_revision != expected.target_revision
    {
        return None;
    }
    if selected_wid != target.wid {
        target.wid = selected_wid;
        target.consecutive_failures = 0;
        target.next_attempt = Instant::now();
        target.target_revision = next_focused_prewarm_target_revision();
        log_debug!(
            "[thumb] focused prewarm target switched to live AX window pid={} wid={}",
            target.pid,
            target.wid
        );
    }
    target.needs_resolution = false;
    Some(target.clone())
}

fn run_focused_prewarm(epoch: u64) {
    crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Utility);
    let mut epoch = epoch;
    let mut wait_for = Duration::from_millis(FOCUSED_PREWARM_INTERVAL_MS);
    loop {
        let guard = FOCUSED_PREWARM_WAKE.0.lock().unwrap();
        let _ = FOCUSED_PREWARM_WAKE
            .1
            .wait_timeout(guard, wait_for)
            .unwrap();
        wait_for = Duration::from_millis(FOCUSED_PREWARM_INTERVAL_MS);
        let current_epoch = FOCUSED_PREWARM_EPOCH.load(Ordering::Acquire);
        if current_epoch != epoch {
            if focused_prewarm_enabled() {
                epoch = current_epoch;
            } else {
                return;
            }
        }
        if !focused_prewarm_enabled() {
            // Leave STARTED owned until finish_focused_prewarm_worker performs the generation-checked
            // handoff. Clearing it here would let immediate re-enable race with this exit and
            // strand the enabled target without a worker.
            // 在 finish_focused_prewarm_worker 做完代际检查和交接前保留 STARTED；此处清除会让
            // 立即重新开启与退出竞态，导致已开启目标没有 worker。
            return;
        }
        if crate::performance::switcher_interaction_active() || !capture_allowed() {
            continue;
        }

        let Some(frontmost_pid) = current_frontmost_pid() else {
            continue;
        };
        let Some(target) = FOCUSED_PREWARM_TARGET.lock().unwrap().clone() else {
            continue;
        };
        if target.pid != frontmost_pid {
            continue;
        }
        let last_capture = CACHE
            .lock()
            .unwrap()
            .peek(&ThumbKey {
                pid: target.pid,
                wid: target.wid,
            })
            .map(|thumb| thumb.captured);
        let now = Instant::now();
        if !target.needs_resolution && !focused_prewarm_due(now, last_capture, target.next_attempt)
        {
            // 捕获在独立 worker 上异步完成，完成时间通常比本线程上次入队晚几十毫秒；
            // 下一轮按真实截止时间补等这段差值，避免固定 5 秒轮询错过后整轮变成 10 秒。
            // Capture finishes asynchronously a few milliseconds after this thread enqueues it.
            // Wait only until the exact freshness deadline so a near miss does not turn a
            // five-second interval into ten seconds.
            wait_for = focused_prewarm_ready_at(last_capture, target.next_attempt)
                .saturating_duration_since(now);
            continue;
        }

        // Re-enumerate the switchable AX windows and current CG geometry. This rejects a
        // disappeared/thin helper surface and follows an in-app window change without trusting
        // the stale focus-tracking id alone.
        // 重新枚举 AX 切换窗口和当前 CG 几何；淘汰消失或过细的辅助窗口，并跟随应用内窗口
        // 切换，不再单独信任可能过期的焦点跟踪 id。
        let Some(window) =
            crate::window_collector::switchable_capture_window_for_pid(frontmost_pid, target.wid)
        else {
            note_focused_prewarm_failure(
                ThumbKey {
                    pid: target.pid,
                    wid: target.wid,
                },
                "no captureable switchable window",
            );
            continue;
        };
        let selected_wid = window.window_id;
        let Some(target) = update_focused_prewarm_target_if_current(&target, selected_wid) else {
            log_debug!(
                "[thumb] focused prewarm target changed during window enumeration; capture skipped"
            );
            continue;
        };
        // Recheck the revision immediately before enqueueing. The target mutex is released
        // before entering CAPTURE_STATE to preserve the existing lock order.
        // 入队前再次检查 revision；进入 CAPTURE_STATE 前释放目标锁，保持现有锁顺序。
        if !focused_prewarm_revision_is_current(Some(target.target_revision)) {
            continue;
        }
        // Hidden prewarm deliberately stays at the baseline thumbnail height; summon/activation
        // paths separately request the larger display-specific target when the user needs it.
        // 隐藏状态预热固定使用基准缩略图高度；召唤/激活路径在用户需要时再请求显示器对应的高清尺寸。
        let target_px_h = cached_target_px_height(target.pid, target.wid).min(BASE_TARGET_PX_H);
        let _ = enqueue_focused_prewarm_job(&target, target_px_h);
    }
}

pub(super) fn enqueue_activation_job(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    activated_at: Instant,
    pid_generation: u64,
) -> bool {
    let key = ThumbKey { pid, wid };
    let tx = ensure_capture_worker();
    let request = CAPTURE_STATE.lock().unwrap().request_activation(
        key,
        target_px_h,
        activated_at,
        pid_generation,
    );
    if matches!(request, ActivationRequestResult::Rejected) {
        return false;
    }
    THUMB_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    if matches!(tx.try_send(()), Err(flume::TrySendError::Disconnected(_))) {
        if matches!(request, ActivationRequestResult::Enqueued) {
            CAPTURE_STATE.lock().unwrap().desired.remove(&key);
        }
        return false;
    }
    true
}

/// Enqueue a delayed blank-frame retry only while its slot still belongs to this window.
/// The lifecycle lock is acquired before the retry-slot lock so window destruction can race
/// safely with the sleeper: either the retry is queued and then invalidated, or it is skipped.
/// 只有重试名额仍属于该窗口时才入队；先拿生命周期锁再拿名额锁，和销毁清理保持一致，
/// 这样延迟线程与窗口销毁并发时，要么入队后被失效，要么直接跳过。
pub(super) fn enqueue_blank_retry_job(
    key: ThumbKey,
    target_px_h: u32,
    activated_at: Instant,
    pid_generation: u64,
) -> Option<BlankRetryEnqueueResult> {
    let tx = ensure_capture_worker();
    let mut state = CAPTURE_STATE.lock().unwrap();
    if !PENDING_BLANK_RETRIES.lock().unwrap().contains(&key) {
        return None;
    }
    let request = state.request_activation(key, target_px_h, activated_at, pid_generation);
    if matches!(request, ActivationRequestResult::Rejected) {
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
        return Some(BlankRetryEnqueueResult::Dropped);
    }
    let merged_running = matches!(request, ActivationRequestResult::Merged { running: true });
    if merged_running {
        // 运行中的任务已经拿走了自己的 CaptureJob，无法再接管 blank_retry 标记；
        // 让它继续完成，但当前延迟重试名额必须立即归还。
        // A running task already owns its copied CaptureJob and cannot take over the
        // blank_retry marker; let it finish, but release this delayed slot now.
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
        return Some(BlankRetryEnqueueResult::Merged);
    }
    let Some(pending) = state.desired.get_mut(&key) else {
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
        return Some(BlankRetryEnqueueResult::Dropped);
    };
    pending.blank_retry = true;
    THUMB_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    if matches!(tx.try_send(()), Err(flume::TrySendError::Disconnected(_))) {
        if matches!(request, ActivationRequestResult::Enqueued) {
            state.desired.remove(&key);
        }
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
        return Some(BlankRetryEnqueueResult::Dropped);
    }
    Some(if matches!(request, ActivationRequestResult::Enqueued) {
        BlankRetryEnqueueResult::Enqueued
    } else {
        BlankRetryEnqueueResult::Merged
    })
}

fn enqueue_job_inner(
    pid: i32,
    wid: u32,
    target_px_h: u32,
    priority: CapturePriority,
    expected_generation: Option<u64>,
    appearance_refresh: bool,
    focused_target_revision: Option<u64>,
) -> bool {
    let key = ThumbKey { pid, wid };
    let tx = ensure_capture_worker();
    let accepted = {
        let mut state = CAPTURE_STATE.lock().unwrap();
        // 无预期 generation 时按当前值解析(等价于原 request();terminated 判定仍在
        // request_for_generation 内生效)。
        // Without an expected generation, resolve the current one (equivalent to the
        // old request(); the terminated check still applies inside
        // request_for_generation).
        let generation = expected_generation.unwrap_or_else(|| state.pid_generation(key.pid));
        let accepted = state.request_for_generation(
            key,
            target_px_h,
            priority,
            generation,
            appearance_refresh,
        );
        if let Some(target_revision) = focused_target_revision {
            state.set_focused_target_revision(key, target_revision);
        }
        accepted
    };
    if !accepted {
        return false;
    }
    THUMB_ENQUEUED.fetch_add(1, Ordering::Relaxed);
    if matches!(tx.try_send(()), Err(flume::TrySendError::Disconnected(_))) {
        CAPTURE_STATE.lock().unwrap().desired.remove(&key);
        return false;
    }
    true
}

fn ensure_capture_worker() -> &'static flume::Sender<()> {
    JOB_TX.get_or_init(|| {
        let (tx, rx) = flume::bounded::<()>(1);
        let worker_tx = tx.clone();
        std::thread::Builder::new()
            .name("thumb-capture".into())
            .spawn(move || {
                crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Utility);
                log_debug!("[thumb] capture worker online");
                for () in rx.iter() {
                    let interaction_active = crate::performance::switcher_interaction_active();
                    let drain_started = Instant::now();
                    let mut drained_jobs = 0usize;
                    // 一个 wake 令牌只负责启动一次 drain;bounded channel 会合并后续
                    // wake,因此必须在同一轮持续消费 CaptureState,否则启动预热只会处理
                    // 前一两项,其余任务虽仍在 desired 中却再也收不到令牌。
                    // One wake token starts a drain. Because the bounded channel coalesces
                    // later wakes, keep consuming CaptureState in this round; otherwise
                    // startup prewarm processes only the first couple of jobs while the rest
                    // remain in `desired` with no token left to wake the worker.
                    loop {
                        let now = Instant::now();
                        let Some(job) = CAPTURE_STATE
                            .lock()
                            .unwrap()
                            .take_next_for_at(interaction_active, now)
                        else {
                            if interaction_active {
                                let pending_background = CAPTURE_STATE
                                    .lock()
                                    .unwrap()
                                    .desired
                                    .values()
                                    .any(|pending| {
                                        !pending.running
                                            && pending.priority < CapturePriority::Visible
                                            && CaptureState::geometry_retry_ready(pending, now)
                                    });
                                if pending_background {
                                    let deferred =
                                        THUMB_INTERACTION_DEFERRED.fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                    if deferred == 1 || deferred.is_multiple_of(16) {
                                        log_debug!(
                                            "[perf] thumbnail background work deferred during interaction count={}",
                                            deferred
                                        );
                                    }
                                }
                            }
                            break;
                        };
                        // Measure only the time spent waiting until this attempt became ready;
                        // capture execution time is recorded separately by run_capture_job.
                        // 只统计任务 ready 后到 worker 取出的等待时间,捕获耗时单独统计。
                        record_thumb_queue_wait(
                            Instant::now()
                                .saturating_duration_since(job.ready_since)
                                .as_millis() as u64,
                        );
                        match run_capture_job(job) {
                            CaptureJobResult::Finished => {
                                let mut state = CAPTURE_STATE.lock().unwrap();
                                let _ = state.finish(job);
                                if job.blank_retry {
                                    state.clear_blank_retry_marker(job);
                                }
                                drop(state);
                                if job.blank_retry {
                                    PENDING_BLANK_RETRIES.lock().unwrap().remove(&job.key);
                                }
                            }
                            CaptureJobResult::BlankRetryScheduled => {
                                let mut state = CAPTURE_STATE.lock().unwrap();
                                let _ = state.finish(job);
                                // The delayed retry owns the slot now; clear the marker on the
                                // completed job so a merged follow-up cannot release that slot.
                                // 延迟重试已接管名额;清掉已完成任务的标记,避免合入的后续任务
                                // 误释放仍属于延迟重试的名额。
                                state.clear_blank_retry_marker(job);
                            }
                            CaptureJobResult::GeometryDeferred => {
                                let result = CAPTURE_STATE.lock().unwrap().defer_geometry_transition(
                                    job,
                                    Instant::now(),
                                );
                                match result {
                                    GeometryDeferResult::Deferred(deadline) => {
                                        let deferred = THUMB_GEOMETRY_DEFERRED
                                            .fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                        if deferred == 1 || deferred.is_multiple_of(16) {
                                            log_debug!(
                                                "[geometry-transition] thumbnail capture deferred during WindowServer geometry transition count={} retry_attempt={}",
                                                deferred,
                                                deadline.attempt
                                            );
                                        }
                                        if !schedule_geometry_retry(deadline, worker_tx.clone()) {
                                            let mut state = CAPTURE_STATE.lock().unwrap();
                                            let removed = state.discard_deferred(job);
                                            state.clear_blank_retry_marker(job);
                                            drop(state);
                                            if job.blank_retry {
                                                PENDING_BLANK_RETRIES.lock().unwrap().remove(&job.key);
                                            }
                                            log_debug!(
                                                "[geometry-transition] retry scheduling failed pid={} wid={} deferred_job_removed={}",
                                                job.key.pid,
                                                job.key.wid,
                                                removed
                                            );
                                        }
                                    }
                                    GeometryDeferResult::Exhausted => {
                                        if job.blank_retry {
                                            let mut state = CAPTURE_STATE.lock().unwrap();
                                            state.clear_blank_retry_marker(job);
                                            drop(state);
                                        }
                                        if job.blank_retry {
                                            PENDING_BLANK_RETRIES.lock().unwrap().remove(&job.key);
                                        }
                                        let exhausted = THUMB_GEOMETRY_RETRY_EXHAUSTED
                                            .fetch_add(1, Ordering::Relaxed)
                                            + 1;
                                        log_debug!(
                                            "[geometry-transition] thumbnail capture retry budget exhausted pid={} wid={} elapsed_ms={} count={}",
                                            job.key.pid,
                                            job.key.wid,
                                            job.enqueued_at.elapsed().as_millis(),
                                            exhausted
                                        );
                                    }
                                    GeometryDeferResult::Stale => {
                                        if job.blank_retry {
                                            let mut state = CAPTURE_STATE.lock().unwrap();
                                            state.clear_blank_retry_marker(job);
                                            drop(state);
                                        }
                                        if job.blank_retry {
                                            PENDING_BLANK_RETRIES.lock().unwrap().remove(&job.key);
                                        }
                                        log_debug!(
                                            "[geometry-transition] stale deferred thumbnail job dropped pid={} wid={}",
                                            job.key.pid,
                                            job.key.wid
                                        );
                                    }
                                }
                            }
                        }
                        drained_jobs += 1;
                    }
                    if drained_jobs > 0 {
                        let (pending, in_flight, ready) = capture_pipeline_stats();
                        let (cache_items, cache_bytes) = cache_stats();
                        log_debug!(
                            "[perf] thumbnail drain complete jobs={} elapsed_ms={} pending={} in_flight={} ready={} cache_items={} cache_bytes={}",
                            drained_jobs,
                            drain_started.elapsed().as_millis(),
                            pending,
                            in_flight,
                            ready,
                            cache_items,
                            cache_bytes,
                        );
                        log_capture_metrics("drain");
                        crate::mem::log_debug_snapshot("thumb-drain-complete");
                    }
                }
            })
            .expect("spawn thumb-capture worker");
        tx
    })
}

fn run_capture_job(job: CaptureJob) -> CaptureJobResult {
    let key = job.key;
    if !CAPTURE_STATE.lock().unwrap().is_current(job) {
        log_debug!(
            "[thumb] job skipped stale pid={} wid={} priority={}",
            key.pid,
            key.wid,
            job.priority.label()
        );
        return CaptureJobResult::Finished;
    }
    if job.priority == CapturePriority::FocusedPrewarm
        && crate::performance::switcher_interaction_active()
    {
        return CaptureJobResult::Finished;
    }
    if job.priority == CapturePriority::FocusedPrewarm {
        if !focused_prewarm_revision_is_current(job.focused_target_revision) {
            log_debug!(
                "[thumb] focused prewarm job skipped after target revision changed pid={} wid={}",
                key.pid,
                key.wid
            );
            return CaptureJobResult::Finished;
        }
        // The live target was selected from the AX switchable set plus current geometry. Do not
        // require AXFocusedWindow to remain equal here: browsers can report a thin helper as
        // focused while their real content window is the capture target.
        // 目标已由 AX 可切换集合和当前几何共同选出；这里不能要求 AXFocusedWindow 仍完全相等，
        // 因为浏览器可能把细条辅助窗口报告为焦点，而真正内容窗口才是捕获目标。
        if !pid_is_frontmost(key.pid) {
            clear_focused_prewarm_if(key, "app-no-longer-frontmost");
            return CaptureJobResult::Finished;
        }
    }
    // 在真正捕获前再次探测，覆盖任务取出后到调用 WindowServer 之间的动画竞态。
    // Probe again immediately before capture to cover the race between job selection
    // and the WindowServer call when the animation starts.
    let geometry = window_server_geometry_transition_snapshot();
    if geometry_capture_should_defer(&geometry, key.wid) {
        let anomaly = geometry
            .abnormal_details
            .get(&key.wid)
            .copied()
            .unwrap_or_default();
        log_debug!(
            "[geometry-transition] capture deferred during WindowServer geometry transition pid={} wid={} priority={} global_active={} target_abnormal={} center_shift={} center_residual=({:.1},{:.1}) transform_changed={} clipped={}",
            key.pid,
            key.wid,
            job.priority.label(),
            geometry.active,
            geometry.abnormal_windows.contains(&key.wid),
            anomaly.center_shift,
            anomaly.center_residual.0,
            anomaly.center_residual.1,
            anomaly.transform_changed,
            anomaly.heavily_clipped
        );
        return CaptureJobResult::GeometryDeferred;
    }
    // 每个任务前重新 preflight:未授权时静默跳过(运行中授权后自动恢复)。
    // Re-preflight per job: silently skip while unauthorized (auto-resumes once
    // granted mid-run).
    let allowed = capture_allowed();
    let enabled = crate::theme::thumbnails_enabled();
    if !allowed || !enabled {
        log_debug!(
            "[thumb] job skipped (allowed={}, enabled={})",
            allowed,
            enabled
        );
        return CaptureJobResult::Finished;
    }
    if job
        .activation_at
        .is_some_and(|activated_at| !activation_capture_is_valid_now(key.pid, activated_at))
    {
        log_debug!(
            "[thumb] activation job skipped after losing frontmost pid={} wid={}",
            key.pid,
            key.wid
        );
        return CaptureJobResult::Finished;
    }
    let job_started = Instant::now();
    let Some(captured) = (unsafe { capture_window(key.wid, job.target_px_h) }) else {
        record_thumb_capture_failed();
        if job.priority == CapturePriority::FocusedPrewarm {
            note_focused_prewarm_failure(key, "capture-failed");
        }
        log_debug!(
            "[thumb] capture failed pid={} wid={} priority={}",
            key.pid,
            key.wid,
            job.priority.label()
        );
        return CaptureJobResult::Finished;
    };
    log_debug!(
        "[thumb] capture result pid={} wid={} source={}x{} cached={}x{} target_h={} priority={}",
        key.pid,
        key.wid,
        captured.source_w_px,
        captured.source_h_px,
        captured.thumb.w_px,
        captured.thumb.h_px,
        job.target_px_h,
        job.priority.label()
    );
    if let Some(reason) = capture_geometry_reject_reason(key, &captured) {
        unsafe {
            CFRelease(captured.thumb.img);
        }
        record_geometry_rejection(reason);
        if job.priority == CapturePriority::FocusedPrewarm {
            note_focused_prewarm_failure(key, reason.label());
        }
        log_debug!(
            "[thumb] captured result discarded by geometry guard pid={} wid={} source={}x{} reason={} priority={}",
            key.pid,
            key.wid,
            captured.source_w_px,
            captured.source_h_px,
            reason.label(),
            job.priority.label()
        );
        return CaptureJobResult::Finished;
    }
    if job
        .activation_at
        .is_some_and(|activated_at| !activation_capture_is_valid_now(key.pid, activated_at))
    {
        unsafe {
            CFRelease(captured.thumb.img);
        }
        log_debug!(
            "[thumb] activation result discarded after losing frontmost pid={} wid={}",
            key.pid,
            key.wid
        );
        return CaptureJobResult::Finished;
    }
    record_thumb_capture(job_started.elapsed().as_millis() as u64);
    // 空白帧门控:后台挂起的 WKWebView(Tauri/Electron 等)截出来只剩"标题栏+
    // 纯色内容",这样的帧不应覆盖缓存里的最后一张有效帧;前台窗口的空白是
    // 用户眼前的真实画面,如实保留。
    // Blank-frame gating: a background-suspended WKWebView (Tauri/Electron et al.)
    // captures as title bar + solid content only; such a frame must never clobber
    // the cached last-known-good image. A blank frontmost window is real and is
    // stored as-is.
    if unsafe { frame_blankness(captured.thumb.img, captured.thumb.w_px, captured.thumb.h_px) }
        .unwrap_or(false)
    {
        let frontmost = pid_is_frontmost(key.pid);
        let cache_has_frame = CACHE.lock().unwrap().peek(&key).is_some();
        let retry_slot_acquired = job.activation_at.is_some()
            && frontmost
            && cache_has_frame
            && PENDING_BLANK_RETRIES.lock().unwrap().insert(key);
        match blank_frame_action(
            frontmost,
            job.activation_at.is_some(),
            cache_has_frame,
            retry_slot_acquired,
            job.appearance_refresh,
        ) {
            BlankFrameAction::Store => {}
            BlankFrameAction::StoreSeed => {
                log_debug!(
                    "[thumb] blank first frame stored as placeholder seed pid={} wid={}",
                    key.pid,
                    key.wid
                );
            }
            BlankFrameAction::StoreAppearanceRefresh => {
                log_debug!(
                    "[thumb] blank frame stored for appearance refresh pid={} wid={}",
                    key.pid,
                    key.wid
                );
            }
            BlankFrameAction::DiscardKeepLastGood => {
                unsafe {
                    CFRelease(captured.thumb.img);
                }
                log_debug!(
                    "[thumb] blank frame discarded, keeping last-known-good pid={} wid={} priority={}",
                    key.pid,
                    key.wid,
                    job.priority.label()
                );
                if job.priority == CapturePriority::FocusedPrewarm {
                    note_focused_prewarm_failure(key, "blank-background");
                }
                return CaptureJobResult::Finished;
            }
            BlankFrameAction::DiscardRetryActivation => {
                unsafe {
                    CFRelease(captured.thumb.img);
                }
                schedule_blank_activation_retry(job);
                log_debug!(
                    "[thumb] blank activation frame discarded, retry scheduled pid={} wid={}",
                    key.pid,
                    key.wid
                );
                if job.priority == CapturePriority::FocusedPrewarm {
                    note_focused_prewarm_failure(key, "blank-activation");
                }
                return CaptureJobResult::BlankRetryScheduled;
            }
        }
    }
    // 生命周期校验与缓存写入共用 CAPTURE_STATE 锁。终止路径按同一锁序取消任务并
    // 清缓存，因此结果不可能在 Remove 之后重新插入。
    // Validate lifecycle and write the cache while holding CAPTURE_STATE. Termination
    // takes the same lock before cancellation/cache eviction, so a result cannot be
    // inserted again after removal.
    let mut state = CAPTURE_STATE.lock().unwrap();
    if !state.is_current(job) {
        unsafe {
            CFRelease(captured.thumb.img);
        }
        log_debug!(
            "[thumb] captured result discarded stale pid={} wid={} priority={}",
            key.pid,
            key.wid,
            job.priority.label()
        );
        return CaptureJobResult::Finished;
    }
    // 只有真正携带 blank_retry 归属标记的任务才能释放名额;普通激活任务不能误删别人的名额。
    // Only a task carrying the explicit blank_retry ownership marker may release the slot;
    // an ordinary activation task must not clear another task's slot.
    if job.blank_retry {
        PENDING_BLANK_RETRIES.lock().unwrap().remove(&key);
        state.clear_blank_retry_marker(job);
    }
    cache_store(key.pid, key.wid, captured.thumb);
    if job.priority == CapturePriority::FocusedPrewarm {
        note_focused_prewarm_success(key);
    }
    drop(state);
    // 不再按任务来源预先决定是否投递：启动预热也可能在浮窗打开后才完成。
    // Do not decide delivery from the request source: startup pre-generation may
    // also finish after the overlay has opened.
    // 结果统一交给主线程做可见性和卡片存在性校验。捕获 worker 不再读取 TAB_STATE，
    // 避免后台线程直接观察主线程运行时状态；隐藏浮窗时主线程会快速丢弃这批通知。
    // Let the main thread validate visibility and card membership. The capture worker no longer
    // reads TAB_STATE, and the main-thread handler quickly drops notifications while hidden.
    enqueue_ready_delivery(key);
    CaptureJobResult::Finished
}

/// 激活补拍的有效性:激活 token 未过时,且该 App 此刻仍是系统前台。
/// Activation refresh validity: the activation token is current AND the app is
/// still the system-frontmost one right now.
pub(super) fn activation_capture_is_valid_now(pid: i32, activated_at: Instant) -> bool {
    crate::window_collector::app_activation_is_current(pid, activated_at) && pid_is_frontmost(pid)
}

/// 查询 NSWorkspace 当前前台 App 是否就是指定 PID。捕获 worker 与延迟补拍线程
/// 都会调用;NSWorkspace 的这类只读消息发送线程安全,不依赖 AppKit 主线程。
/// Whether NSWorkspace currently reports the given PID as the frontmost app.
/// Called from the capture worker and delayed refresh threads; these read-only
/// NSWorkspace messages are thread-safe and do not require the AppKit main thread.
pub(super) fn pid_is_frontmost(pid: i32) -> bool {
    unsafe {
        let pool: *mut AnyObject = msg_send![class!(NSAutoreleasePool), new];
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let frontmost = if app.is_null() {
            false
        } else {
            let frontmost_pid: i32 = msg_send![app, processIdentifier];
            frontmost_pid == pid
        };
        let _: () = msg_send![pool, drain];
        frontmost
    }
}

/// 主线程回调入口(controller 的 thumbnailReady:):清空待投递队列,逐键校验后
/// 就地重建对应卡片。生成期间用户可能已 ↑↓ 或关浮窗,每键都要重新校验。
/// Main-thread callback entry (the controller's thumbnailReady:): drains the
/// pending queue, re-verifies each key, and rebuilds the affected cards in place.
/// The user may have arrowed away or closed the overlay mid-generation, so every
/// key is re-verified.
pub(crate) fn handle_ready_main() {
    if !crate::theme::thumbnails_enabled() {
        READY_QUEUE.lock().unwrap().clear();
        READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
        return;
    }
    // scheduled=false 与 drain 必须在同一队列锁内完成：否则 worker 可能在两步之间
    // 看到旧的 true、放入新 key 却不再安排回调。
    // Clear scheduled and drain under the same queue lock. Otherwise a worker can
    // observe the old true between those steps, append a key, and leave it without
    // a future callback.
    let keys: Vec<ThumbKey> = {
        let mut ready = READY_QUEUE.lock().unwrap();
        let keys = std::mem::take(&mut *ready);
        READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
        keys
    };
    if keys.is_empty() {
        return;
    }
    let ready_batch_size = keys.len();
    let visible = crate::overlay::thumbnail_visible_range();
    let keys: HashSet<ThumbKey> = keys.into_iter().collect();
    let Some(ready_keys) = crate::with_tab_state(|state_opt| {
        let state = state_opt.as_ref()?;
        if !state.visible {
            return None;
        }
        Some(
            state
                .windows
                .iter()
                .enumerate()
                .filter(|(index, window)| {
                    keys.contains(&ThumbKey {
                        pid: window.pid,
                        wid: window.window_id,
                    }) && visible.as_ref().is_none_or(|range| range.contains(index))
                })
                .map(|(_, window)| (window.pid, window.window_id))
                .collect::<Vec<_>>(),
        )
    }) else {
        return;
    };
    if !ready_keys.is_empty() {
        log_debug!(
            "[perf] thumbnail ready batch={} matched_visible={}",
            ready_batch_size,
            ready_keys.len()
        );
        crate::overlay::refresh_thumbnail_previews(&ready_keys);
    }
}

pub(super) static READY_QUEUE: Mutex<Vec<ThumbKey>> = Mutex::new(Vec::new());
pub(super) static READY_DELIVERY_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// 多个 worker 完成通知共享一个主线程 selector；handler 一次清空当前 key 批次。
/// Multiple worker completions share one outstanding main-thread selector; the
/// handler drains the current key batch in one pass.
fn enqueue_ready_delivery(key: ThumbKey) {
    READY_QUEUE.lock().unwrap().push(key);
    if READY_DELIVERY_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    let ctrl = match *crate::CONTROLLER.lock().unwrap() {
        Some(c) => c.0,
        None => {
            READY_DELIVERY_SCHEDULED.store(false, Ordering::Release);
            return;
        }
    };
    unsafe {
        let _: () = msg_send![
            ctrl,
            performSelectorOnMainThread: sel!(thumbnailReady:),
            withObject: std::ptr::null::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

/// 截取一个窗口:CGSHWCaptureWindowList(count=1)→ 取首张 CGImage →
/// 等比缩到目标像素高(降内存:Retina 原生帧可达数十 MB)。
/// Capture one window: CGSHWCCaptureWindowList (count=1) -> first CGImage ->
/// proportionally downscale to the target pixel height (native retina frames can
/// reach tens of MB).
pub(super) struct CapturedWindow {
    source_w_px: u32,
    source_h_px: u32,
    pub(super) thumb: CachedThumb,
}

pub(super) unsafe fn capture_window(wid: u32, target_px_h: u32) -> Option<CapturedWindow> {
    let cap = *CGS_CAPTURE_LIST.as_ref()?;
    // 连接 ID 进程内恒定,缓存一次;0 = 获取失败(私有符号缺失)。
    // The connection ID is process-wide constant; cache it once (0 = unavailable).
    let cid = *CONNECTION_ID.get_or_init(|| skylight::cgs_main_connection().unwrap_or(0));
    if cid == 0 {
        return None;
    }
    let wids = [wid];
    // 显式请求 Retina 原生像素；nominalResolution 只给逻辑点尺寸，小窗口在 4K/5K
    // 屏上放大后仍会发糊，即使后续目标高度提高也无法补回源细节。
    // Explicitly request native Retina pixels. nominalResolution only returns point-sized
    // content, so small windows stay blurry on 4K/5K displays even with a larger target later.
    let opts = CGS_CAPTURE_BEST_RESOLUTION | CGS_CAPTURE_IGNORE_GLOBAL_CLIP_SHAPE;
    let arr = cap(cid, wids.as_ptr(), 1, opts);
    if arr.is_null() {
        return None;
    }
    let n = CFArrayGetCount(arr);
    let raw = if n > 0 {
        CFArrayGetValueAtIndex(arr, 0)
    } else {
        std::ptr::null()
    };
    if raw.is_null() {
        CFRelease(arr);
        return None;
    }
    CFRetain(raw); // 数组即将释放,自留一份 / the array goes away; keep our own ref
    CFRelease(arr);
    let src_w = CGImageGetWidth(raw) as u32;
    let src_h = CGImageGetHeight(raw) as u32;
    let target_px_h = target_px_h.clamp(BASE_TARGET_PX_H, MAX_TARGET_PX_H);
    let (tw, th) = fit_target(src_w, src_h, target_px_h);
    let img = if tw == src_w && th == src_h {
        raw
    } else {
        let scaled = downscale_cgimage(raw, tw, th);
        CFRelease(raw);
        if scaled.is_null() {
            return None;
        }
        scaled
    };
    if img.is_null() {
        return None;
    }
    Some(CapturedWindow {
        source_w_px: src_w,
        source_h_px: src_h,
        thumb: CachedThumb {
            img,
            w_px: tw,
            h_px: th,
            captured_for_px_h: target_px_h,
            captured: Instant::now(),
            // 占位值;实际版本号由 cache_store 统一分配。
            // Placeholder; the real version is assigned centrally in cache_store.
            epoch: 0,
        },
    })
}

/// CGBitmapContext 重绘降采样(纯 CoreGraphics,线程安全;方向与原图一致)。
/// Downscale by redrawing through a CGBitmapContext (pure CoreGraphics,
/// thread-safe; orientation matches the source).
unsafe fn downscale_cgimage(src: *const c_void, tw: u32, th: u32) -> *const c_void {
    if tw == 0 || th == 0 {
        return std::ptr::null();
    }
    let cs = DEVICE_RGB_COLOR_SPACE.ptr;
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        tw as usize,
        th as usize,
        8,
        (tw as usize) * 4,
        cs,
        BITMAP_PREMULTIPLIED_LAST,
    );
    if ctx.is_null() {
        return std::ptr::null();
    }
    CGContextDrawImage(
        ctx,
        CGRect {
            x: 0.0,
            y: 0.0,
            w: tw as f64,
            h: th as f64,
        },
        src,
    );
    let out = CGBitmapContextCreateImage(ctx);
    CFRelease(ctx);
    out
}

static CONNECTION_ID: OnceLock<u32> = OnceLock::new();
