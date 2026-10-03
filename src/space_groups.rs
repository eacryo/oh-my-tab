//! Fullscreen Space ownership by the ordinary desktop from which a window entered fullscreen.
//!
//! Actual WindowServer membership and inferred fullscreen origin are kept separate: origin only
//! extends a desktop's candidate group after a leave/join event pair is confirmed.

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::log_debug;

pub(crate) type SpaceId = u64;
pub(crate) type DisplayId = String;

// A window's ordinary->fullscreen transition is only usable while both edges are close in time.
// The upper bound must cover the delay between the membership events and the Space being typed
// fullscreen by a later query (observed ~0.5s on macOS 26), so it is deliberately wider than the
// event burst itself.
const MEMBERSHIP_PAIR_WINDOW: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SpaceKind {
    #[default]
    Unknown,
    Ordinary,
    Fullscreen,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DisplaySpaces {
    pub(crate) current: SpaceId,
    pub(crate) spaces: HashMap<SpaceId, SpaceKind>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Topology {
    pub(crate) displays: HashMap<DisplayId, DisplaySpaces>,
}

impl Topology {
    pub(crate) fn current_spaces_differ(&self, other: &Self) -> bool {
        if self.displays.is_empty() {
            return false;
        }
        self.displays.len() != other.displays.len()
            || self.displays.iter().any(|(display_id, display)| {
                other
                    .displays
                    .get(display_id)
                    .is_none_or(|other_display| other_display.current != display.current)
            })
    }

    pub(crate) fn space_displays(&self, space_id: SpaceId) -> HashSet<DisplayId> {
        self.displays
            .iter()
            .filter(|(_, spaces)| spaces.spaces.contains_key(&space_id))
            .map(|(display, _)| display.clone())
            .collect()
    }

    pub(crate) fn kind(&self, space_id: SpaceId) -> SpaceKind {
        self.displays
            .values()
            .find_map(|display| display.spaces.get(&space_id).copied())
            .unwrap_or_default()
    }

    /// The ordinary Space that defines the visible switcher group for this display. A fullscreen
    /// Space resolves only through a confirmed origin; otherwise it remains an isolated context.
    pub(crate) fn context_space(
        &self,
        display_id: &str,
        fullscreen_origins: &HashMap<SpaceId, Origin>,
    ) -> Option<SpaceId> {
        let current = self.displays.get(display_id)?.current;
        if self.kind(current) == SpaceKind::Fullscreen {
            fullscreen_origins
                .get(&current)
                .filter(|origin| origin.display_id == display_id)
                .map(|origin| origin.ordinary_space)
                .or(Some(current))
        } else {
            Some(current)
        }
    }

    /// All actual Spaces allowed for one display's switcher group. Unknown fullscreen Spaces
    /// include only themselves; ordinary Spaces include only full-screen Spaces with a confirmed
    /// source on this same display.
    pub(crate) fn allowed_spaces(
        &self,
        display_id: &str,
        fullscreen_origins: &HashMap<SpaceId, Origin>,
    ) -> HashSet<SpaceId> {
        let Some(context) = self.context_space(display_id, fullscreen_origins) else {
            return HashSet::new();
        };
        let mut allowed = HashSet::from([context]);
        if self.kind(context) == SpaceKind::Ordinary {
            allowed.extend(
                fullscreen_origins
                    .iter()
                    .filter_map(|(fullscreen, origin)| {
                        (origin.display_id == display_id && origin.ordinary_space == context)
                            .then_some(*fullscreen)
                    }),
            );
        }
        allowed
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct WindowIdentity {
    pub(crate) pid: i32,
    pub(crate) process_start_time_us: Option<u64>,
    pub(crate) window_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    pub(crate) display_id: DisplayId,
    pub(crate) ordinary_space: SpaceId,
    pub(crate) window: WindowIdentity,
}

#[derive(Clone, Debug)]
struct MembershipEdge {
    space_id: SpaceId,
    at: Instant,
}

#[derive(Default)]
pub(crate) struct Tracker {
    topology: Topology,
    actual_memberships: HashMap<WindowIdentity, HashSet<SpaceId>>,
    pending_leaves: HashMap<WindowIdentity, MembershipEdge>,
    pending_joins: HashMap<WindowIdentity, MembershipEdge>,
    fullscreen_origins: HashMap<SpaceId, Origin>,
    generation: u64,
    evidence_contiguous: bool,
}

impl Tracker {
    pub(crate) fn topology(&self) -> &Topology {
        &self.topology
    }

    pub(crate) fn fullscreen_origins(&self) -> &HashMap<SpaceId, Origin> {
        &self.fullscreen_origins
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn evidence_contiguous(&self) -> bool {
        self.evidence_contiguous
    }

    pub(crate) fn observe_topology_if_generation(
        &mut self,
        expected_generation: u64,
        topology: Topology,
        complete_observation: bool,
    ) -> bool {
        if self.generation != expected_generation {
            return false;
        }
        self.observe_topology(topology);
        if complete_observation {
            self.evidence_contiguous = true;
        }
        true
    }

    pub(crate) fn observe_topology(&mut self, mut topology: Topology) {
        for (display_id, display) in &mut topology.displays {
            let Some(previous) = self.topology.displays.get(display_id) else {
                continue;
            };
            for (space_id, kind) in &mut display.spaces {
                if *kind == SpaceKind::Unknown {
                    if let Some(previous_kind) = previous.spaces.get(space_id) {
                        *kind = *previous_kind;
                    }
                }
            }
        }
        let before = topology_signature(&self.topology);
        self.topology = topology;
        let after = topology_signature(&self.topology);
        if before != after {
            log_debug!("[spaces] topology changed: {}", after);
        }
        self.fullscreen_origins.retain(|fullscreen, origin| {
            self.topology.kind(*fullscreen) == SpaceKind::Fullscreen
                && self.topology.kind(origin.ordinary_space) == SpaceKind::Ordinary
                && self.topology.displays.contains_key(&origin.display_id)
        });
        self.retry_pending(Instant::now());
        self.generation = self.generation.wrapping_add(1);
    }

    pub(crate) fn membership_delta(
        &mut self,
        window: WindowIdentity,
        space_id: SpaceId,
        added: bool,
        at: Instant,
    ) {
        let members = self.actual_memberships.entry(window.clone()).or_default();
        if added {
            members.insert(space_id);
            self.pending_joins
                .insert(window.clone(), MembershipEdge { space_id, at });
        } else {
            members.remove(&space_id);
            self.pending_leaves
                .insert(window.clone(), MembershipEdge { space_id, at });
        }
        log_debug!(
            "[spaces] membership window={} pid={} space={} joined={} process_identity={}",
            window.window_id,
            window.pid,
            space_id,
            added,
            window.process_start_time_us.is_some()
        );
        self.try_associate(&window, at);
        self.generation = self.generation.wrapping_add(1);
    }

    /// Record that the membership event stream had a gap we could not attribute. This only
    /// downgrades the diagnostic flag: discarding pending leave/join pairs here would let an
    /// unrelated window's unresolvable event erase a valid transition for another window, which
    /// is exactly what a fullscreen transition's auxiliary windows produce.
    pub(crate) fn note_evidence_gap(&mut self) {
        self.evidence_contiguous = false;
    }

    /// Drop all pending evidence. Reserved for a known event loss (queue overflow), where a
    /// window's own final leave may have been lost and a stale earlier leave could otherwise be
    /// paired with a later fullscreen join.
    pub(crate) fn mark_discontinuous(&mut self) {
        self.pending_leaves.clear();
        self.pending_joins.clear();
        self.evidence_contiguous = false;
        self.generation = self.generation.wrapping_add(1);
    }

    pub(crate) fn remove_window(&mut self, window_id: u32) {
        self.actual_memberships
            .retain(|identity, _| identity.window_id != window_id);
        self.pending_leaves
            .retain(|identity, _| identity.window_id != window_id);
        self.pending_joins
            .retain(|identity, _| identity.window_id != window_id);
        self.generation = self.generation.wrapping_add(1);
    }

    pub(crate) fn remove_process(&mut self, pid: i32) {
        self.actual_memberships
            .retain(|identity, _| identity.pid != pid);
        self.pending_leaves
            .retain(|identity, _| identity.pid != pid);
        self.pending_joins.retain(|identity, _| identity.pid != pid);
        self.generation = self.generation.wrapping_add(1);
    }

    #[cfg(test)]
    pub(crate) fn window_belongs_to_current_group(&self, window: &WindowIdentity) -> bool {
        let Some(memberships) = self.actual_memberships.get(window) else {
            return false;
        };
        self.topology.displays.keys().any(|display_id| {
            let allowed = self
                .topology
                .allowed_spaces(display_id, &self.fullscreen_origins);
            memberships.iter().any(|space| allowed.contains(space))
        })
    }

    fn retry_pending(&mut self, now: Instant) {
        let candidates: Vec<_> = self
            .pending_leaves
            .keys()
            .chain(self.pending_joins.keys())
            .cloned()
            .collect();
        for window in candidates {
            self.try_associate(&window, now);
        }
        self.pending_leaves
            .retain(|_, edge| now.saturating_duration_since(edge.at) <= MEMBERSHIP_PAIR_WINDOW);
        self.pending_joins
            .retain(|_, edge| now.saturating_duration_since(edge.at) <= MEMBERSHIP_PAIR_WINDOW);
    }

    fn try_associate(&mut self, window: &WindowIdentity, now: Instant) {
        if window.process_start_time_us.is_none() {
            log_debug!(
                "[spaces] association skipped: window={} pid={} has no process identity",
                window.window_id,
                window.pid
            );
            return;
        }
        let (Some(leave), Some(join)) = (
            self.pending_leaves.get(window),
            self.pending_joins.get(window),
        ) else {
            return;
        };
        let leave_kind = self.topology.kind(leave.space_id);
        let join_kind = self.topology.kind(join.space_id);
        if leave.space_id == join.space_id
            || now.saturating_duration_since(leave.at) > MEMBERSHIP_PAIR_WINDOW
            || now.saturating_duration_since(join.at) > MEMBERSHIP_PAIR_WINDOW
            || leave_kind != SpaceKind::Ordinary
            || join_kind != SpaceKind::Fullscreen
        {
            // Only a genuine ordinary->fullscreen pair is worth reporting; the transient
            // same-space pairs that every Space switch produces would otherwise flood the log.
            if leave.space_id != join.space_id {
                log_debug!(
                    "[spaces] association skipped: window={} pid={} leave={} join={} leave_kind={:?} join_kind={:?} leave_age_ms={} join_age_ms={}",
                    window.window_id,
                    window.pid,
                    leave.space_id,
                    join.space_id,
                    leave_kind,
                    join_kind,
                    now.saturating_duration_since(leave.at).as_millis(),
                    now.saturating_duration_since(join.at).as_millis()
                );
            }
            return;
        }
        let source_displays = self.topology.space_displays(leave.space_id);
        let target_displays = self.topology.space_displays(join.space_id);
        let common: Vec<_> = source_displays
            .intersection(&target_displays)
            .cloned()
            .collect();
        if common.len() != 1 {
            log_debug!(
                "[spaces] association skipped: window={} pid={} leave_displays={:?} fullscreen_displays={:?}",
                window.window_id,
                window.pid,
                source_displays,
                target_displays
            );
            return;
        }
        let display_id = common[0].clone();
        let origin = Origin {
            display_id,
            ordinary_space: leave.space_id,
            window: window.clone(),
        };
        match self.fullscreen_origins.get(&join.space_id) {
            Some(existing) if existing.ordinary_space != origin.ordinary_space => {
                log_debug!(
                    "[spaces] association conflict: fullscreen={} existing_desktop={} new_desktop={}",
                    join.space_id,
                    existing.ordinary_space,
                    origin.ordinary_space
                );
                return;
            }
            Some(_) => {}
            None => {
                log_debug!(
                    "[spaces] learned fullscreen origin display={} desktop={} fullscreen={} window={} pid={}",
                    origin.display_id,
                    origin.ordinary_space,
                    join.space_id,
                    origin.window.window_id,
                    origin.window.pid
                );
                self.fullscreen_origins.insert(join.space_id, origin);
            }
        }
        self.pending_leaves.remove(window);
        self.pending_joins.remove(window);
    }
}

static TRACKER: LazyLock<Mutex<Tracker>> = LazyLock::new(|| Mutex::new(Tracker::default()));

fn topology_signature(topology: &Topology) -> String {
    let mut displays: Vec<_> = topology.displays.iter().collect();
    displays.sort_by(|a, b| a.0.cmp(b.0));
    displays
        .into_iter()
        .map(|(display_id, display)| {
            let mut spaces: Vec<_> = display.spaces.iter().collect();
            spaces.sort_by_key(|(space_id, _)| **space_id);
            let spaces = spaces
                .into_iter()
                .map(|(space_id, kind)| format!("{space_id}:{kind:?}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("{display_id}/current={}/[{}]", display.current, spaces)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn with_tracker<T>(f: impl FnOnce(&Tracker) -> T) -> T {
    f(&TRACKER.lock().unwrap())
}

pub(crate) fn with_tracker_mut<T>(f: impl FnOnce(&mut Tracker) -> T) -> T {
    f(&mut TRACKER.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::{DisplaySpaces, Origin, SpaceKind, Topology, Tracker, WindowIdentity};
    use std::collections::{HashMap, HashSet};
    use std::time::{Duration, Instant};

    fn topology() -> Topology {
        Topology {
            displays: HashMap::from([
                (
                    "display-a".into(),
                    DisplaySpaces {
                        current: 10,
                        spaces: HashMap::from([
                            (10, SpaceKind::Ordinary),
                            (11, SpaceKind::Ordinary),
                            (100, SpaceKind::Fullscreen),
                            (101, SpaceKind::Fullscreen),
                        ]),
                    },
                ),
                (
                    "display-b".into(),
                    DisplaySpaces {
                        current: 21,
                        spaces: HashMap::from([
                            (20, SpaceKind::Ordinary),
                            (21, SpaceKind::Ordinary),
                            (200, SpaceKind::Fullscreen),
                        ]),
                    },
                ),
            ]),
        }
    }

    fn identity(pid: i32, window_id: u32) -> WindowIdentity {
        WindowIdentity {
            pid,
            process_start_time_us: Some(pid as u64),
            window_id,
        }
    }

    #[test]
    fn current_space_change_detection_ignores_initialization_and_type_only_updates() {
        let previous = topology();
        assert!(!Topology::default().current_spaces_differ(&previous));
        let mut type_update = previous.clone();
        type_update
            .displays
            .get_mut("display-a")
            .unwrap()
            .spaces
            .insert(100, SpaceKind::Unknown);
        assert!(!previous.current_spaces_differ(&type_update));
        let mut moved = previous.clone();
        moved.displays.get_mut("display-a").unwrap().current = 11;
        assert!(previous.current_spaces_differ(&moved));
    }

    #[test]
    fn ordinary_desktop_includes_only_its_confirmed_fullscreen_spaces() {
        let mut tracker = Tracker::default();
        let a_fullscreen = identity(1, 101);
        let b_fullscreen = identity(2, 201);
        let origin_a = Origin {
            display_id: "display-a".into(),
            ordinary_space: 10,
            window: a_fullscreen.clone(),
        };
        let origin_b = Origin {
            display_id: "display-b".into(),
            ordinary_space: 20,
            window: b_fullscreen.clone(),
        };
        tracker.topology = topology();
        tracker.fullscreen_origins.insert(100, origin_a);
        tracker.fullscreen_origins.insert(200, origin_b);
        tracker.actual_memberships = HashMap::from([
            (identity(3, 301), HashSet::from([10])),
            (a_fullscreen, HashSet::from([100])),
            (identity(4, 401), HashSet::from([11])),
            (b_fullscreen, HashSet::from([200])),
        ]);
        assert!(tracker.window_belongs_to_current_group(&identity(3, 301)));
        assert!(tracker.window_belongs_to_current_group(&identity(1, 101)));
        assert!(!tracker.window_belongs_to_current_group(&identity(4, 401)));
        assert!(!tracker.window_belongs_to_current_group(&identity(2, 201)));
    }

    #[test]
    fn active_fullscreen_space_resolves_to_its_source_desktop_group() {
        let mut topology = topology();
        topology.displays.get_mut("display-a").unwrap().current = 100;
        let fullscreen = identity(1, 101);
        let origins = HashMap::from([(
            100,
            Origin {
                display_id: "display-a".into(),
                ordinary_space: 10,
                window: fullscreen.clone(),
            },
        )]);
        let mut tracker = Tracker::default();
        tracker.topology = topology;
        tracker.fullscreen_origins = origins;
        tracker.actual_memberships = HashMap::from([
            (identity(2, 201), HashSet::from([10])),
            (fullscreen.clone(), HashSet::from([100])),
            (identity(3, 301), HashSet::from([20])),
        ]);
        assert!(tracker.window_belongs_to_current_group(&identity(2, 201)));
        assert!(tracker.window_belongs_to_current_group(&fullscreen));
        assert!(!tracker.window_belongs_to_current_group(&identity(3, 301)));
    }

    #[test]
    fn unknown_fullscreen_context_does_not_leak_other_desktops() {
        let mut topology = topology();
        topology.displays.get_mut("display-a").unwrap().current = 101;
        let mut tracker = Tracker::default();
        tracker.topology = topology;
        tracker.actual_memberships = HashMap::from([
            (identity(1, 101), HashSet::from([101])),
            (identity(2, 201), HashSet::from([200])),
            (identity(3, 301), HashSet::from([10])),
        ]);
        assert!(tracker.window_belongs_to_current_group(&identity(1, 101)));
        assert!(!tracker.window_belongs_to_current_group(&identity(2, 201)));
        assert!(!tracker.window_belongs_to_current_group(&identity(3, 301)));
    }

    #[test]
    fn leave_and_join_events_confirm_origin_in_either_order() {
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let id = identity(1, 101);
        let now = Instant::now();
        tracker.membership_delta(id.clone(), 10, false, now);
        tracker.membership_delta(id.clone(), 100, true, now + Duration::from_millis(1));
        assert_eq!(
            tracker
                .fullscreen_origins
                .get(&100)
                .map(|origin| origin.ordinary_space),
            Some(10)
        );
        let id2 = identity(2, 202);
        tracker.membership_delta(id2.clone(), 200, true, now + Duration::from_millis(2));
        tracker.membership_delta(id2.clone(), 20, false, now + Duration::from_millis(3));
        assert_eq!(
            tracker
                .fullscreen_origins
                .get(&200)
                .map(|origin| origin.ordinary_space),
            Some(20)
        );
    }

    #[test]
    fn conflicting_source_evidence_does_not_reassign_a_fullscreen_space() {
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let now = Instant::now();
        let first = identity(1, 101);
        tracker.membership_delta(first.clone(), 10, false, now);
        tracker.membership_delta(first, 100, true, now);
        let second = identity(2, 201);
        tracker.membership_delta(second.clone(), 11, false, now);
        tracker.membership_delta(second, 100, true, now);
        assert_eq!(
            tracker
                .fullscreen_origins
                .get(&100)
                .map(|origin| origin.ordinary_space),
            Some(10)
        );
    }

    #[test]
    fn incomplete_process_identity_cannot_confirm_an_origin() {
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let identity = WindowIdentity {
            pid: 1,
            process_start_time_us: None,
            window_id: 101,
        };
        let now = Instant::now();
        tracker.membership_delta(identity.clone(), 10, false, now);
        tracker.membership_delta(identity, 100, true, now);
        assert!(!tracker.fullscreen_origins.contains_key(&100));
    }

    #[test]
    fn incomplete_topology_does_not_downgrade_a_known_fullscreen_space() {
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let partial = Topology {
            displays: HashMap::from([(
                "display-a".into(),
                DisplaySpaces {
                    current: 10,
                    spaces: HashMap::from([(10, SpaceKind::Unknown), (100, SpaceKind::Unknown)]),
                },
            )]),
        };
        tracker.observe_topology(partial);
        assert_eq!(tracker.topology.kind(100), SpaceKind::Fullscreen);
    }

    #[test]
    fn an_unattributed_event_gap_does_not_discard_another_windows_pair() {
        // A fullscreen transition makes the OS emit membership events for auxiliary windows whose
        // owner is not in the subscription index yet. Those events used to clear every pending
        // pair, so the real transition was never learned. Only a known event loss may do that.
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let id = identity(1, 101);
        let now = Instant::now();
        tracker.membership_delta(id.clone(), 10, false, now);
        tracker.note_evidence_gap();
        tracker.membership_delta(id.clone(), 100, true, now + Duration::from_millis(1));
        assert_eq!(
            tracker
                .fullscreen_origins
                .get(&100)
                .map(|origin| origin.ordinary_space),
            Some(10)
        );
        assert!(!tracker.evidence_contiguous());
    }

    #[test]
    fn a_known_event_loss_discards_pending_pairs() {
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let id = identity(1, 101);
        let now = Instant::now();
        tracker.membership_delta(id.clone(), 10, false, now);
        tracker.mark_discontinuous();
        tracker.membership_delta(id, 100, true, now + Duration::from_millis(1));
        assert!(tracker.fullscreen_origins.is_empty());
    }

    #[test]
    fn a_pair_survives_a_space_typed_fullscreen_only_after_the_join() {
        // The 1325 join can arrive while the new space is still typed ordinary; the kind is
        // corrected by a later query, at which point the retained pair must be retried.
        let mut tracker = Tracker::default();
        let mut lagging = topology();
        lagging
            .displays
            .get_mut("display-a")
            .unwrap()
            .spaces
            .insert(101, SpaceKind::Ordinary);
        tracker.topology = lagging;
        let id = identity(1, 101);
        let now = Instant::now();
        tracker.membership_delta(id.clone(), 10, false, now);
        tracker.membership_delta(id, 101, true, now + Duration::from_millis(500));
        assert!(tracker.fullscreen_origins.is_empty());
        tracker.observe_topology(topology());
        assert_eq!(
            tracker
                .fullscreen_origins
                .get(&101)
                .map(|origin| origin.ordinary_space),
            Some(10)
        );
    }

    #[test]
    fn removing_origin_window_keeps_the_group_until_the_space_disappears() {
        let mut tracker = Tracker::default();
        tracker.topology = topology();
        let id = identity(1, 101);
        tracker.fullscreen_origins.insert(
            100,
            Origin {
                display_id: "display-a".into(),
                ordinary_space: 10,
                window: id.clone(),
            },
        );
        tracker.actual_memberships.insert(id, HashSet::from([100]));
        tracker.remove_window(101);
        assert!(tracker.fullscreen_origins.contains_key(&100));
        assert!(tracker.actual_memberships.is_empty());
        tracker.observe_topology(Topology::default());
        assert!(tracker.fullscreen_origins.is_empty());
    }
}
