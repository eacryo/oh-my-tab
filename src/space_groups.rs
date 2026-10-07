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

/// Where a window's actual Space memberships put it, relative to what the switcher shows (see
/// `Topology::membership_scope`). This is a *location*, not a verdict: the admission policy
/// (`collect::admits_space_scope`) decides which locations are candidates.
///
/// The contract since 2026-10-07: the current Space's windows and every fullscreen Space's windows
/// are always candidates; another ordinary desktop's windows need the "always show other desktops"
/// switch, or a fullscreen Space being current. The fullscreen Space's *origin desktop* deliberately
/// plays no part -- see `docs/fullscreen-space-groups-plan.md` for why (the origin is a learned or
/// inferred association, never a fact macOS exposes, so admission must not depend on it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MembershipScope {
    #[default]
    Unknown,
    /// On a display's current Space.
    CurrentSpace,
    /// On a fullscreen Space, whatever desktop it was created from.
    FullscreenSpace,
    /// On an ordinary Space that is not the current one.
    OtherDesktop,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DisplaySpaces {
    pub(crate) current: SpaceId,
    pub(crate) spaces: HashMap<SpaceId, SpaceKind>,
    /// The native Space order for this display (the index order `CGSCopyManagedDisplaySpaces`
    /// returns). macOS keeps a fullscreen Space immediately after the ordinary Space it was created
    /// from, which is what `inferred_origin` reads; the map above keeps the kinds.
    pub(crate) ordered: Vec<SpaceId>,
}

impl DisplaySpaces {
    /// The ordinary Space a fullscreen Space is *adjacent to* in the native order: the nearest
    /// preceding ordinary Space on the same display.
    ///
    /// This is a heuristic, not a recovered historical fact. Measured 2026-10-07 with "Automatically
    /// rearrange Spaces based on most recent use" at its default (on): the list was `[1 ordinary,
    /// 430 fullscreen, 386 ordinary]` and 430 had been fullscreened from desktop 1, so the fullscreen
    /// Space sat next to the desktop it came from even though desktops reorder. Nothing in the
    /// snapshot promises that, though: if the order ever becomes `[D1, D2, F]`, a fresh process
    /// groups F with D2 (`a_reordered_space_list_groups_by_adjacency` pins that behaviour).
    ///
    /// It is only a fallback for the *learned* association (see `Tracker::effective_origin`), so a
    /// wrong guess is replaced by the first observation, and a fullscreen Space with no preceding
    /// ordinary Space (or one whose neighbours are all unknown) stays isolated as before.
    pub(crate) fn inferred_origin(&self, fullscreen: SpaceId) -> Option<SpaceId> {
        let index = self.ordered.iter().position(|space| *space == fullscreen)?;
        self.ordered[..index]
            .iter()
            .rev()
            .find(|space| self.spaces.get(space) == Some(&SpaceKind::Ordinary))
            .copied()
    }
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

    /// Whether this Space belongs to the accepted display topology. A membership query can also
    /// name Spaces the topology does not manage (a stale record, or a Space from a display that is
    /// gone), and those must not count as evidence that a window lives on another desktop.
    pub(crate) fn space_is_managed(&self, space_id: SpaceId) -> bool {
        self.displays
            .values()
            .any(|display| display.spaces.contains_key(&space_id))
    }

    /// How a window's actual Space memberships relate to the switcher's candidate scope.
    ///
    /// `Unknown` is not "no windows": an empty membership list and an unmanaged Space both mean
    /// the same thing here -- there is no accepted evidence about where this window lives, which
    /// is also the shape an orderOut'd surface presents. Only `CurrentGroup` admits a window
    /// under today's rule; `OtherDesktop` is the evidence the "other desktops" switch requires.
    pub(crate) fn membership_scope(&self, memberships: &[SpaceId]) -> MembershipScope {
        let mut managed = false;
        let mut fullscreen = false;
        for space_id in memberships {
            if !self.space_is_managed(*space_id) {
                continue;
            }
            managed = true;
            // The current Space wins over the rest: a window can be on several Spaces at once
            // (sticky / "All Desktops" windows), and then it is simply here.
            if self
                .displays
                .values()
                .any(|display| display.current == *space_id)
            {
                return MembershipScope::CurrentSpace;
            }
            if self.kind(*space_id) == SpaceKind::Fullscreen {
                fullscreen = true;
            }
        }
        if fullscreen {
            MembershipScope::FullscreenSpace
        } else if managed {
            MembershipScope::OtherDesktop
        } else {
            MembershipScope::Unknown
        }
    }

    /// Whether any display is currently showing a fullscreen Space.
    ///
    /// The contract widens the candidate scope to every desktop while the user is inside a
    /// fullscreen app ("show all desktops' windows + fullscreen windows"), which is what the
    /// admission policy reads this for.
    pub(crate) fn any_current_space_is_fullscreen(&self) -> bool {
        self.displays
            .values()
            .any(|display| self.kind(display.current) == SpaceKind::Fullscreen)
    }

    /// The origin governing a fullscreen Space on this display: the learned association when an
    /// observation confirmed one, otherwise the native Space order's nearest preceding ordinary
    /// Space (see `DisplaySpaces::inferred_origin`). `None` means no origin is known.
    ///
    /// Diagnostics only since 2026-10-07: admission no longer consults the origin (see
    /// `MembershipScope`), so a wrong or missing association cannot hide a window. Kept because the
    /// e2e state publishes it and because a future feature may want it back.
    pub(crate) fn effective_origin(
        &self,
        display_id: &str,
        fullscreen: SpaceId,
        fullscreen_origins: &HashMap<SpaceId, Origin>,
    ) -> Option<SpaceId> {
        fullscreen_origins
            .get(&fullscreen)
            .filter(|origin| origin.display_id == display_id)
            .map(|origin| origin.ordinary_space)
            .or_else(|| {
                self.displays
                    .get(display_id)
                    .and_then(|display| display.inferred_origin(fullscreen))
            })
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
            // Say where each fullscreen Space's origin came from: a learned association, or the
            // native Space order (see `DisplaySpaces::inferred_origin`).
            let inferred: Vec<String> = self
                .topology
                .displays
                .iter()
                .flat_map(|(display_id, display)| {
                    // Borrow only what the closure needs: `self` is borrowed by the caller.
                    let learned = &self.fullscreen_origins;
                    display.spaces.iter().filter_map(move |(space, kind)| {
                        if *kind != SpaceKind::Fullscreen || learned.contains_key(space) {
                            return None;
                        }
                        Some(match display.inferred_origin(*space) {
                            Some(origin) => format!("{display_id}:{space}->{origin}"),
                            None => format!("{display_id}:{space}->none"),
                        })
                    })
                })
                .collect();
            if !inferred.is_empty() {
                log_debug!(
                    "[spaces] fullscreen origins by native order: {:?}",
                    inferred
                );
            }
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

/// Whether any display is currently showing a fullscreen Space (see
/// `Topology::any_current_space_is_fullscreen`). Read per collection pass for the admission policy.
pub(crate) fn current_space_is_fullscreen() -> bool {
    with_tracker(|tracker| tracker.topology().any_current_space_is_fullscreen())
}

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
    use super::{
        DisplaySpaces, MembershipScope, Origin, SpaceKind, Topology, Tracker, WindowIdentity,
    };
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
                        ordered: vec![10, 11, 100, 101],
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
                        ordered: vec![20, 21, 200],
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
    fn membership_scope_reports_where_a_window_is() {
        let topology = topology();
        // No member, or a Space the accepted topology does not manage: no evidence at all. The
        // unmanaged id is the shape a stale record (or an orderOut'd surface) presents, so it must
        // never be read as "this window lives somewhere".
        for memberships in [vec![], vec![999], vec![999, 998]] {
            assert_eq!(
                topology.membership_scope(&memberships),
                MembershipScope::Unknown,
                "{memberships:?} must not be accepted evidence"
            );
        }
        // The current Space of either display.
        for memberships in [vec![10], vec![21]] {
            assert_eq!(
                topology.membership_scope(&memberships),
                MembershipScope::CurrentSpace
            );
        }
        // A fullscreen Space is its own location, whatever desktop it came from -- this is the
        // location the contract always admits, which is why the origin association is not needed.
        for memberships in [vec![100], vec![101], vec![200]] {
            assert_eq!(
                topology.membership_scope(&memberships),
                MembershipScope::FullscreenSpace,
                "{memberships:?} is a fullscreen Space"
            );
        }
        // A managed ordinary Space that is not current is another desktop: display-a's 11, and
        // display-b's 20 while 21 is current.
        for memberships in [vec![11], vec![20]] {
            assert_eq!(
                topology.membership_scope(&memberships),
                MembershipScope::OtherDesktop,
                "{memberships:?} is another desktop"
            );
        }
        // A window that is a member of both an inactive and the active Space is judged by its whole
        // membership set: the current Space wins, so it is not hidden behind the switch.
        assert_eq!(
            topology.membership_scope(&[10, 11]),
            MembershipScope::CurrentSpace
        );
        // A sticky window that is on the current Space and on fullscreen Spaces is simply here.
        assert_eq!(
            topology.membership_scope(&[100, 10]),
            MembershipScope::CurrentSpace
        );
    }

    #[test]
    fn a_fullscreen_current_space_widens_the_policy_not_the_location() {
        // The "inside a fullscreen app shows every desktop" rule is a policy decision, so it must
        // not change what the classification reports: another desktop stays another desktop.
        let mut topology = topology();
        assert!(!topology.any_current_space_is_fullscreen());
        topology.displays.get_mut("display-a").unwrap().current = 100;
        assert!(topology.any_current_space_is_fullscreen());
        assert_eq!(
            topology.membership_scope(&[11]),
            MembershipScope::OtherDesktop
        );
        assert_eq!(
            topology.membership_scope(&[100]),
            MembershipScope::CurrentSpace,
            "the fullscreen Space that is current is the current Space"
        );
    }

    #[test]
    fn a_fullscreen_space_infers_its_origin_from_the_native_order() {
        // Measured shape: `[1 ordinary, 430 fullscreen, 386 ordinary]` with 430 fullscreened from
        // desktop 1. The helper's display-a is the same shape: 100/101 follow the ordinary 11.
        let topology = topology();
        let learned = HashMap::new();
        assert_eq!(
            topology.effective_origin("display-a", 100, &learned),
            Some(11)
        );
        assert_eq!(
            topology.effective_origin("display-a", 101, &learned),
            Some(11)
        );
        assert_eq!(topology.effective_origin("display-a", 10, &learned), None);

        let display = &topology.displays["display-a"];
        assert_eq!(
            display.inferred_origin(100),
            Some(11),
            "the nearest preceding ordinary Space is the origin"
        );

        let mut fullscreen_current = topology.clone();
        fullscreen_current
            .displays
            .get_mut("display-a")
            .unwrap()
            .current = 100;

        // An unknown-kind neighbour is not an origin: only an Ordinary Space counts.
        let mut with_unknown = topology.clone();
        {
            let display = with_unknown.displays.get_mut("display-a").unwrap();
            display.spaces.insert(50, SpaceKind::Unknown);
            display.ordered = vec![10, 50, 100];
        }
        assert_eq!(
            with_unknown
                .displays
                .get("display-a")
                .unwrap()
                .inferred_origin(100),
            Some(10)
        );
    }

    #[test]
    fn a_reordered_space_list_groups_by_adjacency() {
        // The documented limitation, pinned rather than hidden: the inference reads the *current*
        // order, so a fullscreen Space that no longer follows its origin desktop is grouped with
        // whatever ordinary Space precedes it now. macOS reorders desktops by recent use
        // (`mru-spaces`, on by default), and nothing in the snapshot records the historical origin.
        let mut topology = topology();
        {
            let display = topology.displays.get_mut("display-a").unwrap();
            // 100 was created from the ordinary 10, but the order now puts it after 11.
            display.ordered = vec![10, 11, 100, 101];
        }
        let learned = HashMap::new();
        assert_eq!(
            topology.effective_origin("display-a", 100, &learned),
            Some(11)
        );
        // 101 follows the fullscreen 100, so the nearest preceding *ordinary* is still 11.
        assert_eq!(
            topology.effective_origin("display-a", 101, &learned),
            Some(11)
        );
        // A learned association is what corrects it once the transition is observed.
        let learned = HashMap::from([(
            100,
            Origin {
                display_id: "display-a".into(),
                ordinary_space: 10,
                window: identity(1, 100),
            },
        )]);
        assert_eq!(
            topology.effective_origin("display-a", 100, &learned),
            Some(10)
        );
    }

    #[test]
    fn a_learned_origin_overrides_the_inferred_one() {
        // The observation is authoritative: when it disagrees with the native order, the learned
        // association decides (and the conflict rule keeps the first observation).
        let topology = topology();
        let learned = HashMap::from([(
            100,
            Origin {
                display_id: "display-a".into(),
                ordinary_space: 10,
                window: identity(1, 100),
            },
        )]);
        assert_eq!(
            topology.effective_origin("display-a", 100, &learned),
            Some(10)
        );
        // The Space without a learned entry still uses the inference.
        assert_eq!(
            topology.effective_origin("display-a", 101, &learned),
            Some(11)
        );
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
                    ordered: vec![10, 100],
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
