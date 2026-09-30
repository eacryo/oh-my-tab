//! Space-change settling state shared by collection, refresh publication, and thumbnail capture.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Tunable settling interval for WindowServer's Space and fullscreen transition snapshots.
pub(crate) const SETTLE_DURATION: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TransitionSnapshot {
    pub(crate) generation: u64,
    pub(crate) active: bool,
    pub(crate) deadline: Option<Instant>,
    /// Absolute Unix time in milliseconds, intended for e2e diagnostics.
    pub(crate) deadline_unix_ms: u64,
}

#[derive(Default)]
pub(crate) struct TransitionScheduler {
    generation: u64,
    settled_generation: u64,
    deadline: Option<Instant>,
    deadline_unix_ms: u64,
}

impl TransitionScheduler {
    pub(crate) fn record_transition(
        &mut self,
        now: Instant,
        now_unix_ms: u64,
    ) -> TransitionSnapshot {
        self.generation = self.generation.wrapping_add(1);
        let deadline = now + SETTLE_DURATION;
        self.deadline = Some(deadline);
        self.deadline_unix_ms = now_unix_ms.saturating_add(SETTLE_DURATION.as_millis() as u64);
        self.snapshot(now)
    }

    pub(crate) fn snapshot(&self, now: Instant) -> TransitionSnapshot {
        let active = self.deadline.is_some_and(|deadline| deadline > now);
        TransitionSnapshot {
            generation: self.generation,
            active,
            deadline: self.deadline,
            deadline_unix_ms: if active { self.deadline_unix_ms } else { 0 },
        }
    }

    /// Complete a transition only once, even if multiple delayed callbacks were queued.
    pub(crate) fn settle_if_due(&mut self, now: Instant) -> bool {
        if self.deadline.is_none_or(|deadline| deadline > now)
            || self.settled_generation == self.generation
        {
            return false;
        }
        self.deadline = None;
        self.deadline_unix_ms = 0;
        self.settled_generation = self.generation;
        true
    }

    pub(crate) fn allows_publication(&self, generation: u64, now: Instant) -> bool {
        generation == self.generation && !self.snapshot(now).active
    }
}

#[derive(Default)]
pub(crate) struct SpaceFlipDetector {
    previous_onscreen: Option<HashSet<u32>>,
    previous_fullscreen: HashSet<u32>,
    previous_started_at: Option<Instant>,
    explicit_transition_at: Option<Instant>,
    explicit_baseline_pending: bool,
}

impl SpaceFlipDetector {
    /// An explicit Workspace notification is authoritative; use its next CG snapshot as the
    /// baseline so the fallback does not add a second debounce after the transition settled.
    pub(crate) fn explicit_transition_seen(&mut self, transition_at: Instant) {
        self.explicit_transition_at = Some(transition_at);
        self.explicit_baseline_pending = true;
    }

    /// Detect fullscreen Space flips even on systems that omit a Workspace notification.
    /// One outgoing plus one incoming window is the smallest fullscreen-to-fullscreen change.
    pub(crate) fn observe(
        &mut self,
        onscreen: HashSet<u32>,
        fullscreen: HashSet<u32>,
        collection_started_at: Instant,
    ) -> bool {
        if self
            .previous_started_at
            .is_some_and(|previous_started_at| collection_started_at < previous_started_at)
        {
            // A slower earlier collection may finish last; it cannot rewind the newer baseline.
            return false;
        }

        if self
            .explicit_transition_at
            .is_some_and(|transition_at| collection_started_at < transition_at)
        {
            self.set_baseline(onscreen, fullscreen, collection_started_at);
            return false;
        }

        if self.explicit_baseline_pending {
            self.set_baseline(onscreen, fullscreen, collection_started_at);
            self.explicit_baseline_pending = false;
            return false;
        }

        let Some(previous_onscreen) = self.previous_onscreen.replace(onscreen.clone()) else {
            self.previous_fullscreen = fullscreen;
            self.previous_started_at = Some(collection_started_at);
            return false;
        };
        let previous_fullscreen =
            std::mem::replace(&mut self.previous_fullscreen, fullscreen.clone());
        self.previous_started_at = Some(collection_started_at);
        if previous_fullscreen.is_empty() && fullscreen.is_empty() {
            return false;
        }
        let changed = previous_onscreen
            .symmetric_difference(&onscreen)
            .take(2)
            .count();
        changed >= 2
    }

    fn set_baseline(
        &mut self,
        onscreen: HashSet<u32>,
        fullscreen: HashSet<u32>,
        collection_started_at: Instant,
    ) {
        self.previous_onscreen = Some(onscreen);
        self.previous_fullscreen = fullscreen;
        self.previous_started_at = Some(collection_started_at);
    }
}

static SCHEDULER: LazyLock<Mutex<TransitionScheduler>> =
    LazyLock::new(|| Mutex::new(TransitionScheduler::default()));
static SPACE_FLIPS: LazyLock<Mutex<SpaceFlipDetector>> =
    LazyLock::new(|| Mutex::new(SpaceFlipDetector::default()));

pub(crate) fn record_transition_at(now: Instant) -> TransitionSnapshot {
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    SCHEDULER
        .lock()
        .unwrap()
        .record_transition(now, now_unix_ms)
}

pub(crate) fn snapshot() -> TransitionSnapshot {
    SCHEDULER.lock().unwrap().snapshot(Instant::now())
}

pub(crate) fn settle_if_due() -> bool {
    SCHEDULER.lock().unwrap().settle_if_due(Instant::now())
}

/// Hold the scheduler lock across snapshot application so a concurrent flip detector cannot
/// mark the transition between the final stale check and publishing the collected window set.
pub(crate) fn while_stable<T>(generation: u64, apply: impl FnOnce() -> T) -> Option<T> {
    let scheduler = SCHEDULER.lock().unwrap();
    if !scheduler.allows_publication(generation, Instant::now()) {
        return None;
    }
    Some(apply())
}

pub(crate) fn note_explicit_transition(transition_at: Instant) {
    SPACE_FLIPS
        .lock()
        .unwrap()
        .explicit_transition_seen(transition_at);
}

pub(crate) fn observe_window_snapshot(
    onscreen: HashSet<u32>,
    fullscreen: HashSet<u32>,
    collection_started_at: Instant,
) -> bool {
    SPACE_FLIPS
        .lock()
        .unwrap()
        .observe(onscreen, fullscreen, collection_started_at)
}

#[cfg(test)]
mod tests {
    use super::{SpaceFlipDetector, TransitionScheduler, SETTLE_DURATION};
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    #[test]
    fn requests_remain_deferred_until_the_latest_settle_deadline() {
        let start = Instant::now();
        let mut scheduler = TransitionScheduler::default();
        let first = scheduler.record_transition(start, 10_000);
        assert!(first.active);
        assert_eq!(first.deadline, Some(start + SETTLE_DURATION));
        assert!(
            scheduler
                .snapshot(start + Duration::from_millis(399))
                .active
        );
        assert!(!scheduler.settle_if_due(start + Duration::from_millis(399)));

        let extended = scheduler.record_transition(start + Duration::from_millis(250), 10_250);
        assert_eq!(extended.deadline, Some(start + Duration::from_millis(650)));
        assert!(
            scheduler
                .snapshot(start + Duration::from_millis(400))
                .active
        );
        assert!(!scheduler.settle_if_due(start + Duration::from_millis(649)));
        assert!(scheduler.settle_if_due(start + Duration::from_millis(650)));
        assert!(
            !scheduler
                .snapshot(start + Duration::from_millis(650))
                .active
        );
        assert!(!scheduler.settle_if_due(start + Duration::from_millis(700)));
    }

    #[test]
    fn snapshot_collected_before_transition_stays_stale_after_settle() {
        let start = Instant::now();
        let mut scheduler = TransitionScheduler::default();
        let collection_generation = scheduler.snapshot(start).generation;
        let transition = scheduler.record_transition(start, 20_000);
        assert!(!scheduler.allows_publication(collection_generation, start));
        assert!(!scheduler.allows_publication(transition.generation, start));
        assert!(scheduler.allows_publication(transition.generation, start + SETTLE_DURATION));
    }

    #[test]
    fn fullscreen_onscreen_flip_fallback_requires_outgoing_and_incoming_windows() {
        let mut detector = SpaceFlipDetector::default();
        let start = Instant::now();
        assert!(!detector.observe(HashSet::from([1, 2]), HashSet::from([1]), start));
        assert!(detector.observe(
            HashSet::from([2, 3]),
            HashSet::from([3]),
            start + Duration::from_millis(1)
        ));

        let mut single_change = SpaceFlipDetector::default();
        assert!(!single_change.observe(HashSet::from([1, 2]), HashSet::from([1]), start));
        assert!(!single_change.observe(
            HashSet::from([1, 2, 3]),
            HashSet::from([1]),
            start + Duration::from_millis(1)
        ));
    }

    #[test]
    fn stale_observations_do_not_consume_explicit_transition_baseline() {
        let start = Instant::now();
        let mut detector = SpaceFlipDetector::default();
        assert!(!detector.observe(HashSet::from([1, 2]), HashSet::from([1]), start));
        let transition_at = start + Duration::from_millis(100);
        detector.explicit_transition_seen(transition_at);
        let mut scheduler = TransitionScheduler::default();
        scheduler.record_transition(transition_at, 20_100);

        // This collection began before the notification but completed afterward. It updates
        // the baseline without consuming the explicit transition marker.
        assert!(!detector.observe(
            HashSet::from([1, 2]),
            HashSet::from([1]),
            start + Duration::from_millis(90)
        ));
        assert_eq!(detector.explicit_transition_at, Some(transition_at));

        // The first post-notification snapshot consumes the pending baseline and becomes the new
        // baseline; the explicit timestamp remains to reject any even later stale completion.
        assert!(!detector.observe(
            HashSet::from([3, 4]),
            HashSet::from([3]),
            start + Duration::from_millis(500)
        ));
        assert_eq!(detector.explicit_transition_at, Some(transition_at));
        assert!(!detector.observe(
            HashSet::from([3, 4]),
            HashSet::from([3]),
            start + Duration::from_millis(510)
        ));
        assert_eq!(scheduler.generation, 1);

        // A delayed stale collection cannot replace the newer baseline or consume another flip.
        assert!(!detector.observe(
            HashSet::from([1, 2]),
            HashSet::from([1]),
            start + Duration::from_millis(95)
        ));
        assert_eq!(detector.previous_onscreen, Some(HashSet::from([3, 4])));

        // A later unannounced fullscreen flip is detected once; an identical follow-up cannot
        // retrigger a second settle.
        let fallback_at = start + Duration::from_millis(520);
        assert!(detector.observe(HashSet::from([5, 6]), HashSet::from([5]), fallback_at));
        scheduler.record_transition(fallback_at, 20_520);
        assert_eq!(scheduler.generation, 2);
        assert!(!detector.observe(
            HashSet::from([5, 6]),
            HashSet::from([5]),
            start + Duration::from_millis(530)
        ));
        assert_eq!(scheduler.generation, 2);
    }
}
