//! In-overlay action keys: ⌘W / ⌘M / ⌘H / ⌘Q acting on the HIGHLIGHTED card.
//!
//! This module is a leaf on purpose. Two very different threads need the same answer: the binding
//! engine's session tap (which must decide whether a press belongs to the panel or to a user
//! binding) and the AppKit main thread (which actually performs the action). Neither may read the
//! other's state, so the main thread publishes one immutable snapshot here and the tap only
//! queries it -- the same shape as `key_bindings::engine::SELF_FRONTMOST`.
//!
//! Contract, settled with the user and the design review:
//! * **Command is required in every trigger mode.** An Option+Tab or Control+Tab user keeps holding
//!   that modifier and adds Command, so the keys stay ⌘W/⌘M/⌘H/⌘Q exactly as promised; Option or
//!   Control is merely tolerated as the panel's own hold modifier in its own mode. Shift is never part
//!   of the chord.
//! * Ownership persists to the physical release: a press the panel claimed keeps its repeats and
//!   its release, so nothing leaks back to the frontmost app mid-hold.
//! * Once claimed, the event is consumed even if the panel became ineligible in between (a config
//!   change on the main thread): the action is merely cancelled. Falling through to `super` there
//!   would let a panel-claimed ⌘Q reach this app's own Quit menu item.
//!
//! Layout: the chord is remembered as a **physical keycode**, resolved on the main thread from the
//! live keyboard layout, because the tap thread must not call Text Input Source APIs. The mapping
//! is rebuilt whenever the input source changes; an unavailable layout falls back to the ANSI
//! positions for w/m/h/q, so the keys still work on an unreadable layout.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use crate::event_tap::keyboard::{FLAG_COMMAND, FLAG_CONTROL, FLAG_OPTION, FLAG_SHIFT};
use crate::log_debug;

/// The four in-overlay actions, in switch order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PanelAction {
    Close,
    Minimize,
    Hide,
    Quit,
}

impl PanelAction {
    /// The character each action is bound to, for the layout lookup.
    pub(crate) const fn character(self) -> char {
        match self {
            Self::Close => 'w',
            Self::Minimize => 'm',
            Self::Hide => 'h',
            Self::Quit => 'q',
        }
    }

    /// The ANSI fallback keycode, used when the layout cannot be read.
    pub(crate) const fn ansi_keycode(self) -> u16 {
        match self {
            Self::Close => 13,    // kVK_ANSI_W
            Self::Minimize => 46, // kVK_ANSI_M
            Self::Hide => 4,      // kVK_ANSI_H
            Self::Quit => 12,     // kVK_ANSI_Q
        }
    }

    pub(crate) const ALL: [Self; 4] = [Self::Close, Self::Minimize, Self::Hide, Self::Quit];
}

/// The effective switches for one config, with the development launch switch applied.
///
/// The SINGLE place the four switches are resolved. The tap lifecycle asks whether any is on, and
/// the snapshot builder asks which; both read this, so the two can never disagree about whether the
/// feature is enabled (the first review round found exactly that drift).
pub(crate) fn switches_for(cfg: &crate::config::Config) -> [bool; 4] {
    let mut switches = [
        cfg.windows.panel_close_enabled,
        cfg.windows.panel_minimize_enabled,
        cfg.windows.panel_hide_enabled,
        cfg.windows.panel_quit_enabled,
    ];
    // Development-verification switch (argv only): enables exactly the named actions for that
    // launch, so the A2 scenario does not have to edit the user's config. Unknown names are ignored
    // rather than arming an action the user never asked for.
    if let Some(value) = crate::dev_flags::value("panel-action-keys") {
        switches = [false; 4];
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            match name {
                "close" => switches[0] = true,
                "minimize" => switches[1] = true,
                "hide" => switches[2] = true,
                "quit" => switches[3] = true,
                _ => log_debug!("[panel] unknown action {:?} ignored", name),
            }
        }
    }
    switches
}

/// Whether any panel action is configured on for this config.
pub(crate) fn configured_in(cfg: &crate::config::Config) -> bool {
    switches_for(cfg).into_iter().any(|on| on)
}

/// Which of the four switches are on, plus the resolved physical keys. Rebuilt whole on every
/// config change or input-source change, so a reader never sees a half-updated map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PanelKeymap {
    close: Option<u16>,
    minimize: Option<u16>,
    hide: Option<u16>,
    quit: Option<u16>,
}

impl PanelKeymap {
    /// Build the ANSI-position map: each enabled action resolves to its ANSI key, which is what
    /// the overlay falls back to when the layout cannot be read.
    pub(crate) fn ansi_fallback(close: bool, minimize: bool, hide: bool, quit: bool) -> Self {
        let mut map = Self::default();
        for (action, on) in [
            (PanelAction::Close, close),
            (PanelAction::Minimize, minimize),
            (PanelAction::Hide, hide),
            (PanelAction::Quit, quit),
        ] {
            map.set(action, on.then(|| action.ansi_keycode()));
        }
        map
    }

    /// Set one action's key (`None` disables it). Used while building a fresh map.
    pub(crate) fn set(&mut self, action: PanelAction, keycode: Option<u16>) {
        match action {
            PanelAction::Close => self.close = keycode,
            PanelAction::Minimize => self.minimize = keycode,
            PanelAction::Hide => self.hide = keycode,
            PanelAction::Quit => self.quit = keycode,
        }
    }

    fn keycode(self, action: PanelAction) -> Option<u16> {
        match action {
            PanelAction::Close => self.close,
            PanelAction::Minimize => self.minimize,
            PanelAction::Hide => self.hide,
            PanelAction::Quit => self.quit,
        }
    }

    /// Whether this action is enabled in this map (its switch was on when the map was built).
    pub(crate) fn is_enabled(self, action: PanelAction) -> bool {
        self.keycode(action).is_some()
    }

    fn action_for_keycode(self, keycode: u16) -> Option<PanelAction> {
        PanelAction::ALL
            .into_iter()
            .find(|action| self.keycode(*action) == Some(keycode))
    }

    pub(crate) fn has_any(&self) -> bool {
        PanelAction::ALL
            .into_iter()
            .any(|action| self.keycode(action).is_some())
    }
}

/// The published state, read as ONE value so visibility, the key map and the tolerated hold
/// modifier can never be observed in a mixed combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct PanelSnapshot {
    visible: bool,
    tolerate_option: bool,
    tolerate_control: bool,
    keymap: PanelKeymap,
}

impl PanelSnapshot {
    pub(crate) fn new(visible: bool, tolerate_option: bool, keymap: PanelKeymap) -> Self {
        Self {
            visible,
            tolerate_option,
            tolerate_control: false,
            keymap,
        }
    }

    /// Also tolerate Control as the held switcher modifier (Control+Tab mode).
    pub(crate) fn tolerating_control(mut self, tolerate_control: bool) -> Self {
        self.tolerate_control = tolerate_control;
        self
    }

    /// The action this key event asks for, if the panel owns it right now.
    fn action(self, keycode: u16, flags: u32) -> Option<PanelAction> {
        if !self.visible {
            return None;
        }
        if !modifiers_match(flags, self.tolerate_option, self.tolerate_control) {
            return None;
        }
        self.keymap.action_for_keycode(keycode)
    }

    pub(crate) fn has_any(&self) -> bool {
        self.visible && self.keymap.has_any()
    }
}

/// The modifier bits that carry any meaning for a chord.
///
/// Only these four are compared. Everything else in the flag word is event metadata or a
/// device-specific marker that must NOT affect the decision:
///   * 0x0001_0000 AlphaShift, 0x0080_0000 SecondaryFn: not chord modifiers; Caps Lock left on must
///     not disable the keys.
///   * 0x0020_0000 NumericPad, 0x0040_0000 Help.
///   * 0x0100_0000 AlphaShiftStateless, 0x0020_0000... see IOLLEvent.h.
///   * 0x0000_0001..0x0000_00xx, 0x0000_2000: the DEVICE-dependent left/right key masks
///     (NX_DEVICELCMDKEYMASK 0x8, NX_DEVICERCMDKEYMASK 0x10, and friends). Real hardware sets the
///     side-specific bit alongside the device-independent one, so a "no unknown bits may remain"
///     check rejected genuine Command chords from a physical keyboard.
///
/// The values are the SDK's own (IOKit/hidsystem/IOLLEvent.h, re-exported as kCGEventFlagMask*).
const CHORD_MODIFIERS: u32 =
    (FLAG_COMMAND as u32) | (FLAG_OPTION as u32) | (FLAG_SHIFT as u32) | (FLAG_CONTROL as u32);

/// Whether the flags mean "Command, optionally plus the panel's own hold modifier".
///
/// The comparison is exact on the four chord modifiers and ignores every other bit, because the
/// rest of the word is event metadata or device markers, never part of what the user pressed.
/// Shift is rejected: Cmd+Shift+W is not this feature's chord. Control is rejected unless it is
/// the tolerated hold modifier of Control+Tab mode.
fn modifiers_match(flags: u32, tolerate_option: bool, tolerate_control: bool) -> bool {
    let modifiers = flags & CHORD_MODIFIERS;
    if modifiers & FLAG_COMMAND as u32 == 0 {
        return false;
    }
    if modifiers & (FLAG_SHIFT as u32) != 0 {
        return false;
    }
    if modifiers & (FLAG_CONTROL as u32) != 0 && !tolerate_control {
        return false;
    }
    let option = modifiers & FLAG_OPTION as u32 != 0;
    if option && !tolerate_option {
        return false;
    }
    true
}

/// The published snapshot slot. The tap clones the `Arc`; the main thread replaces it whole.
static SNAPSHOT: LazyLock<RwLock<Arc<PanelSnapshot>>> =
    LazyLock::new(|| RwLock::new(Arc::new(PanelSnapshot::default())));

/// Physical keys whose press the panel claimed and which are still HELD. Keyed by the physical key
/// alone, like the binding engine's own swallow record, because the user may release a modifier
/// before the main key. A key stays here from its press to its release: repeats consult it instead
/// of re-deciding eligibility, so turning a switch off mid-hold cannot leak the repeats back to the
/// frontmost app.
static HELD_KEYS: LazyLock<Mutex<HashSet<u16>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The actions claimed presses will run, captured at press time and consumed by the AppKit handler.
///
/// This is deliberately SEPARATE from HELD_KEYS. A key's ownership is a property of the physical
/// hold (it ends on release), while the action has to survive until the main thread actually
/// processes the queued key event. Folding them into one set let a fast press-and-release delete the
/// record before AppKit looked, so the handler saw "not claimed", returned false, and a Cmd+Q fell
/// through to the app's own Quit menu item. The captured action also freezes what the press meant:
/// a layout change mid-hold cannot retarget it.
///
/// A QUEUE per keycode, not a single slot: two Cmd+Q presses that both reach the tap before the main
/// thread drains them are two distinct events, each needing its own record. A single slot let the
/// second press overwrite the first, so one event found a record and the other did not.
static PENDING_ACTIONS: LazyLock<Mutex<HashMap<u16, Vec<PendingAction>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// One claimed press waiting for the main thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PendingAction {
    pub(crate) action: PanelAction,
    /// The session the press belonged to. A press that was already queued must not run after the
    /// panel ended that session (Esc, a commit, a new summon): the event is still consumed, but the
    /// action is cancelled.
    pub(crate) session: u64,
}

/// Bumped on every session end. A queued action stamped with an older generation is consumed
/// without running, which is what makes "turn the switch off / dismiss the panel" able to cancel a
/// pending action without letting its key escape to the app.
static SESSION: AtomicU64 = AtomicU64::new(0);

/// The current session generation.
pub(crate) fn session() -> u64 {
    SESSION.load(Ordering::Relaxed)
}

/// End the current session: a later press belongs to a new one. Pending actions from the old
/// generation stay queued (their events still need consuming) but will not run.
pub(crate) fn end_session() {
    SESSION.fetch_add(1, Ordering::Relaxed);
}

/// Whether the panel currently owns any chord at all. Lets the binding engine skip its whole
/// claim path on the common case (feature off / panel hidden).
static PANEL_CLAIMS_ANY: AtomicBool = AtomicBool::new(false);

/// How many panel actions have actually RUN. The smoke uses it to tell "the handler consumed the
/// event" from "the handler consumed it AND ran the action": the consumption return value alone
/// cannot distinguish those, so an implementation that ran a cancelled Cmd+Q would still pass.
static ACTIONS_RUN: AtomicU64 = AtomicU64::new(0);

/// Record that one action ran.
pub(crate) fn note_action_run() {
    ACTIONS_RUN.fetch_add(1, Ordering::Relaxed);
}

/// How many actions have run since launch (diagnostics / smoke).
pub(crate) fn actions_run() -> u64 {
    ACTIONS_RUN.load(Ordering::Relaxed)
}

/// The EFFECTIVE switches and panel-open flag, as last published.
///
/// The diagnostics read these rather than the config, because a development launch switch can
/// override the config for one launch (the A2 scenario relies on that): reporting the config would
/// make the scenario assert a value the running app is not using.
static EFFECTIVE: LazyLock<Mutex<Option<PanelEffective>>> = LazyLock::new(|| Mutex::new(None));

/// One action's effective state for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelEffective {
    pub(crate) visible: bool,
    pub(crate) close: bool,
    pub(crate) minimize: bool,
    pub(crate) hide: bool,
    pub(crate) quit: bool,
}

/// The last published effective state, or all-off when nothing has been published yet.
pub(crate) fn effective() -> PanelEffective {
    EFFECTIVE
        .lock()
        .ok()
        .and_then(|state| *state)
        .unwrap_or(PanelEffective {
            visible: false,
            close: false,
            minimize: false,
            hide: false,
            quit: false,
        })
}

/// Publish a new snapshot. Main thread only; cheap (a pointer swap).
pub(crate) fn publish(snapshot: PanelSnapshot) {
    PANEL_CLAIMS_ANY.store(snapshot.has_any(), Ordering::Relaxed);
    // Record the effective state for diagnostics in the same call, so a reader can never see the
    // claim flag and the switches from different generations.
    if let Ok(mut state) = EFFECTIVE.lock() {
        *state = Some(PanelEffective {
            visible: snapshot.visible,
            close: snapshot.keymap.keycode(PanelAction::Close).is_some(),
            minimize: snapshot.keymap.keycode(PanelAction::Minimize).is_some(),
            hide: snapshot.keymap.keycode(PanelAction::Hide).is_some(),
            quit: snapshot.keymap.keycode(PanelAction::Quit).is_some(),
        });
    }
    if let Ok(mut slot) = SNAPSHOT.write() {
        *slot = Arc::new(snapshot);
    }
    // Deliberately NOT clearing CLAIMED_KEYS here. Ownership is a fact about a physical hold, not
    // about the current switch state: a key pressed while its switch was on is still held, and its
    // release has to be matched. Clearing on a switch change would also break the consumption rule
    // below -- the smoke caught exactly that, with a claimed Cmd+Q falling through to the main menu
    // after its switch went off. The record is dropped on the hold's release and when the session
    // ends (clear_claims from clear_sticky).
}

fn snapshot() -> Arc<PanelSnapshot> {
    match SNAPSHOT.read() {
        Ok(slot) => Arc::clone(&slot),
        Err(_) => Arc::new(PanelSnapshot::default()),
    }
}

/// Whether the panel owns any chord right now. Off-main safe.
pub(crate) fn claims_any() -> bool {
    PANEL_CLAIMS_ANY.load(Ordering::Relaxed)
}

/// The tap's verdict for one key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimDecision {
    /// Not ours: the caller runs its ordinary binding logic.
    NotClaimed,
    /// The panel owns this key: hand the event through and let AppKit deliver it to the panel.
    PassThrough,
    /// The panel owns this key and this is a repeat: swallow it, so one press is one action.
    SwallowRepeat,
}

/// The tap's decision for a press, given whether the binding engine had already swallowed the key.
///
/// Ownership is decided ONCE per physical press:
///  * `binding_already_swallowed`: a key a binding took first is never retroactively reassigned.
///  * already in `HELD_KEYS`: a repeat of a held key stays owned WITHOUT re-checking eligibility.
///    Re-deciding here was the defect the review found -- switching a switch off mid-hold let the
///    repeats leak back to the frontmost app.
///  * otherwise the current snapshot decides, and the action is captured for the main thread.
pub(crate) fn decide_press(
    keycode: u16,
    flags: u32,
    binding_already_swallowed: bool,
    autorepeat: bool,
) -> ClaimDecision {
    // An autorepeat that never established ownership is NOT a new press: it must not be able to
    // create a claim, or a repeat arriving after a session ended would run an action nobody asked
    // for. Repeats only ever answer for a hold that was already claimed.
    if autorepeat {
        let held = match HELD_KEYS.lock() {
            Ok(held) => held,
            Err(_) => return ClaimDecision::NotClaimed,
        };
        return if held.contains(&keycode) {
            ClaimDecision::SwallowRepeat
        } else {
            ClaimDecision::NotClaimed
        };
    }
    let action = {
        let mut held = match HELD_KEYS.lock() {
            Ok(held) => held,
            Err(_) => return ClaimDecision::NotClaimed,
        };
        if held.contains(&keycode) {
            // A second real press without a release: one hold, one action.
            return ClaimDecision::SwallowRepeat;
        }
        if binding_already_swallowed {
            return ClaimDecision::NotClaimed;
        }
        if !claims_any() {
            return ClaimDecision::NotClaimed;
        }
        let Some(action) = snapshot().action(keycode, flags) else {
            return ClaimDecision::NotClaimed;
        };
        held.insert(keycode);
        action
    };
    // One record per claimed press: two presses that both reach the tap before the main thread
    // drains them must each keep their own action.
    if let Ok(mut pending) = PENDING_ACTIONS.lock() {
        pending.entry(keycode).or_default().push(PendingAction {
            action,
            session: session(),
        });
    }
    ClaimDecision::PassThrough
}

/// The tap's decision for a release. `true` means the panel owned the key and the caller must let
/// the release through and skip its own release handling.
///
/// The held record goes, but `PENDING_ACTIONS` is NOT touched: the queued key-down may still be
/// waiting for the main thread, and dropping its action here is what allowed a quick Cmd+Q to fall
/// through to the app's own Quit item. The action is consumed by `take_pending_action`.
pub(crate) fn decide_release(keycode: u16) -> bool {
    let mut held = match HELD_KEYS.lock() {
        Ok(held) => held,
        Err(_) => return false,
    };
    held.remove(&keycode)
}

/// What the main thread found for one key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingOutcome {
    /// The tap never claimed this key: the event is not ours.
    NotOurs,
    /// The tap claimed it, but the action was cancelled (its switch went off, or the session
    /// ended). The event must still be CONSUMED -- returning it to AppKit would let a cancelled
    /// Cmd+Q reach our own Quit menu item -- but no action runs.
    Cancelled,
    /// Run this action.
    Run(PanelAction),
}

/// Take the oldest action queued for `keycode`. Main thread, exactly once per queued press.
///
/// The three outcomes exist because "the tap never claimed this" and "the tap claimed it but the
/// action was cancelled" need OPPOSITE handling of the same event: the first belongs to AppKit, the
/// second must be swallowed. Collapsing them into one `None` made the cancelled case fall through
/// to the app.
pub(crate) fn take_pending_action(keycode: u16) -> PendingOutcome {
    let record = {
        let mut queued = match PENDING_ACTIONS.lock() {
            Ok(queued) => queued,
            Err(_) => return PendingOutcome::NotOurs,
        };
        let Some(records) = queued.get_mut(&keycode) else {
            return PendingOutcome::NotOurs;
        };
        if records.is_empty() {
            return PendingOutcome::NotOurs;
        }
        let record = records.remove(0);
        if records.is_empty() {
            queued.remove(&keycode);
        }
        record
    };
    if record.session != session() {
        log_debug!(
            "[panel] queued {:?} cancelled: it belonged to an earlier session",
            record.action
        );
        return PendingOutcome::Cancelled;
    }
    if !snapshot().keymap.is_enabled(record.action) {
        log_debug!(
            "[panel] queued {:?} cancelled: its switch is off now",
            record.action
        );
        return PendingOutcome::Cancelled;
    }
    PendingOutcome::Run(record.action)
}

/// Whether `keycode` has any queued action. The tap-side clipboard interception uses this.
pub(crate) fn has_pending_action(keycode: u16) -> bool {
    PENDING_ACTIONS
        .lock()
        .map(|pending| {
            pending
                .get(&keycode)
                .is_some_and(|records| !records.is_empty())
        })
        .unwrap_or(false)
}

/// Record a queued action without a real event. Used only by the in-app smoke runner.
///
/// The action is always MINIMIZE: the smoke runs inside the app under test, so a queued Quit could
/// terminate the very process the runner is validating.
pub(crate) fn claim_for_smoke(keycode: u16) {
    if let Ok(mut pending) = PENDING_ACTIONS.lock() {
        pending.entry(keycode).or_default().push(PendingAction {
            action: PanelAction::Minimize,
            session: session(),
        });
    }
}

/// Drop every held key. Used by the HARD teardown (the tap stopping) where an outstanding key-up
/// can no longer arrive.
///
/// It deliberately does NOT touch the queued actions: a press that was already released to AppKit
/// still has to be CONSUMED when the main thread gets to it. Dropping the record would make the
/// handler report `NotOurs`, let the event fall through, and a panel-claimed Cmd+Q would quit this
/// app instead of the selected one. The pending records are cancelled by generation instead
/// (`end_session`), which is what the tap teardown also does.
pub(crate) fn clear_held() {
    if let Ok(mut held) = HELD_KEYS.lock() {
        held.clear();
    }
}

/// Drop every held key AND every queued action. For a smoke run that needs a blank slate: it is NOT
/// the tap-teardown path (see `clear_held`).
pub(crate) fn clear_claims() {
    clear_held();
    if let Ok(mut pending) = PENDING_ACTIONS.lock() {
        pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// These tests exercise ONE process-wide snapshot and claim record, so they must not run
    /// concurrently: cargo runs test functions in parallel threads by default, and two tests
    /// publishing different snapshots would observe each other's state.
    fn state_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn keymap_all() -> PanelKeymap {
        PanelKeymap::ansi_fallback(true, true, true, true)
    }

    fn snapshot_visible(tolerate_option: bool) -> PanelSnapshot {
        PanelSnapshot::new(true, tolerate_option, keymap_all())
    }

    #[test]
    fn a_hidden_panel_claims_nothing() {
        let _guard = state_lock();
        let hidden = PanelSnapshot::new(false, false, keymap_all());
        assert_eq!(hidden.action(13, FLAG_COMMAND as u32), None);
        assert!(!hidden.has_any());
    }

    #[test]
    fn a_disabled_action_claims_nothing() {
        let _guard = state_lock();
        let map = PanelKeymap::ansi_fallback(false, false, false, false);
        assert!(!PanelSnapshot::new(true, false, map).has_any());
        let only_close = PanelKeymap::ansi_fallback(true, false, false, false);
        let snap = PanelSnapshot::new(true, false, only_close);
        assert_eq!(
            snap.action(13, FLAG_COMMAND as u32),
            Some(PanelAction::Close)
        );
        assert_eq!(snap.action(46, FLAG_COMMAND as u32), None);
    }

    #[test]
    fn command_is_required_in_both_trigger_modes() {
        for tolerate_option in [false, true] {
            let snap = snapshot_visible(tolerate_option);
            let bare = 0;
            assert_eq!(snap.action(13, bare), None, "a bare key is not the chord");
            assert_eq!(snap.action(13, FLAG_OPTION as u32), None);
        }
        assert_eq!(
            snapshot_visible(false).action(13, FLAG_COMMAND as u32),
            Some(PanelAction::Close)
        );
    }

    #[test]
    fn the_panel_hold_modifier_is_tolerated_only_in_option_mode() {
        let chord = FLAG_COMMAND as u32 | FLAG_OPTION as u32;
        assert_eq!(snapshot_visible(false).action(13, chord), None);
        assert_eq!(
            snapshot_visible(true).action(13, chord),
            Some(PanelAction::Close)
        );
    }

    #[test]
    fn control_is_tolerated_only_in_control_mode() {
        let _guard = state_lock();
        let chord = FLAG_COMMAND as u32 | FLAG_CONTROL as u32;
        assert_eq!(snapshot_visible(false).action(13, chord), None);
        let control_mode = snapshot_visible(false).tolerating_control(true);
        assert_eq!(control_mode.action(13, chord), Some(PanelAction::Close));
        assert_eq!(
            control_mode.action(13, FLAG_COMMAND as u32),
            Some(PanelAction::Close)
        );
        // Control mode tolerates Control only: Option and Shift still break the chord.
        assert_eq!(control_mode.action(13, chord | FLAG_OPTION as u32), None);
        assert_eq!(control_mode.action(13, chord | FLAG_SHIFT as u32), None);
    }

    #[test]
    fn extra_modifiers_are_never_part_of_the_chord() {
        let _guard = state_lock();
        let command = FLAG_COMMAND as u32;
        let snap = snapshot_visible(true);
        for extra in [FLAG_SHIFT as u32, FLAG_CONTROL as u32] {
            assert_eq!(snap.action(13, command | extra), None);
            assert_eq!(snap.action(13, command | FLAG_OPTION as u32 | extra), None);
        }
    }

    #[test]
    fn caps_lock_and_fn_do_not_break_the_chord() {
        // 0x0001_0000 AlphaShift, 0x0080_0000 SecondaryFn: metadata, not chord modifiers.
        let command = FLAG_COMMAND as u32;
        let snap = snapshot_visible(false);
        assert_eq!(
            snap.action(13, command | 0x0001_0000 | 0x0080_0000),
            Some(PanelAction::Close)
        );
    }

    #[test]
    fn real_hardware_device_bits_do_not_break_the_chord() {
        let _guard = state_lock();
        // NX_DEVICELCMDKEYMASK / NX_DEVICERCMDKEYMASK (0x8 / 0x10): a physical keyboard sets the
        // side-specific bit alongside the device-independent one. A "no unknown bits" check
        // rejected genuine chords from real hardware; this is the counter-example.
        let command = FLAG_COMMAND as u32;
        let snap = snapshot_visible(false);
        for device_bit in [0x0000_0008u32, 0x0000_0010] {
            assert_eq!(
                snap.action(13, command | device_bit),
                Some(PanelAction::Close),
                "left/right device command bit must not break the chord"
            );
        }
    }

    #[test]
    fn event_metadata_bits_never_break_the_chord() {
        let _guard = state_lock();
        // NumericPad 0x0020_0000, Help 0x0040_0000, AlphaShiftStateless 0x0100_0000, NonCoalesced
        // 0x0000_0100 (the SDK values, verified against IOLLEvent.h). None is a chord modifier.
        let command = FLAG_COMMAND as u32;
        let snap = snapshot_visible(false);
        for metadata in [
            0x0020_0000u32,
            0x0040_0000,
            0x0100_0000,
            0x0000_0100,
            0x0000_2000,
        ] {
            assert_eq!(
                snap.action(13, command | metadata),
                Some(PanelAction::Close),
                "metadata bit {metadata:#x} must not break the chord"
            );
        }
    }

    #[test]
    fn each_action_maps_to_its_own_key() {
        let snap = snapshot_visible(false);
        let command = FLAG_COMMAND as u32;
        assert_eq!(snap.action(13, command), Some(PanelAction::Close));
        assert_eq!(snap.action(46, command), Some(PanelAction::Minimize));
        assert_eq!(snap.action(4, command), Some(PanelAction::Hide));
        assert_eq!(snap.action(12, command), Some(PanelAction::Quit));
        assert_eq!(snap.action(99, command), None);
    }

    #[test]
    fn ownership_survives_a_switch_turning_off_mid_hold() {
        let _guard = state_lock();
        // The review's counter-example: a repeat of a key the panel already owns must stay owned
        // even after its switch goes off, or the repeats leak back to the frontmost app.
        clear_claims();
        publish(snapshot_visible(false));
        assert_eq!(
            decide_press(13, FLAG_COMMAND as u32, false, false),
            ClaimDecision::PassThrough
        );
        publish(PanelSnapshot::new(true, false, PanelKeymap::default()));
        assert_eq!(
            decide_press(13, FLAG_COMMAND as u32, false, false),
            ClaimDecision::SwallowRepeat,
            "a held key stays owned: repeats must not be re-decided"
        );
        assert!(decide_release(13), "the release is still ours");
        clear_claims();
    }

    #[test]
    fn a_binding_that_swallowed_the_key_first_keeps_it() {
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        assert_eq!(
            decide_press(13, FLAG_COMMAND as u32, true, false),
            ClaimDecision::NotClaimed,
            "a key a binding took first is never reassigned to the panel"
        );
        clear_claims();
    }

    #[test]
    fn a_release_does_not_drop_the_pending_action() {
        let _guard = state_lock();
        // The failure the smoke caught: the tap must keep the captured action until the main thread
        // consumes it, or a fast press-and-release lets Cmd+Q reach the app's own Quit item.
        clear_claims();
        publish(snapshot_visible(false));
        assert_eq!(
            decide_press(12, FLAG_COMMAND as u32, false, false),
            ClaimDecision::PassThrough
        );
        assert!(decide_release(12), "the release is ours");
        assert!(
            has_pending_action(12),
            "the queued action must survive the physical release"
        );
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::Run(PanelAction::Quit)
        );
        assert!(
            !has_pending_action(12),
            "consuming it retires the record, so one press is one action"
        );
        clear_claims();
    }

    #[test]
    fn one_press_yields_exactly_one_action() {
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        let _ = decide_press(12, FLAG_COMMAND as u32, false, false);
        assert_eq!(
            decide_press(12, FLAG_COMMAND as u32, false, false),
            ClaimDecision::SwallowRepeat
        );
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::Run(PanelAction::Quit)
        );
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::NotOurs,
            "only one action per press"
        );
        clear_claims();
    }

    #[test]
    fn a_hidden_panel_never_claims_a_fresh_press() {
        let _guard = state_lock();
        clear_claims();
        publish(PanelSnapshot::new(false, false, keymap_all()));
        assert_eq!(
            decide_press(13, FLAG_COMMAND as u32, false, false),
            ClaimDecision::NotClaimed
        );
        clear_claims();
    }

    #[test]
    fn two_presses_queued_before_the_main_thread_keep_both_actions() {
        // The review's counter-example: both down/up pairs reach the tap before AppKit processes
        // either event. A single slot per keycode let the second press overwrite the first, so one
        // event found a record and the other fell through to the app's own menu.
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        for _ in 0..2 {
            assert_eq!(
                decide_press(12, FLAG_COMMAND as u32, false, false),
                ClaimDecision::PassThrough
            );
            assert!(decide_release(12));
        }
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::Run(PanelAction::Quit)
        );
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::Run(PanelAction::Quit),
            "the second queued press must still have its own action"
        );
        assert_eq!(take_pending_action(12), PendingOutcome::NotOurs);
        clear_claims();
    }

    #[test]
    fn ending_a_session_cancels_queued_actions_without_dropping_the_hold() {
        // Cancelling must not run the action, and must not release the key either: the user is
        // still physically holding it, so its repeats have to stay owned until the release.
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        let _ = decide_press(12, FLAG_COMMAND as u32, false, false);
        end_session();
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::Cancelled,
            "an action from an ended session must be consumed but not run"
        );
        assert_eq!(
            decide_press(12, FLAG_COMMAND as u32, false, true),
            ClaimDecision::SwallowRepeat,
            "the physical hold survives the session end"
        );
        assert!(decide_release(12), "its release is still ours");
        clear_claims();
    }

    #[test]
    fn a_switch_turned_off_cancels_a_queued_action() {
        // The event is still consumed (so it cannot reach our own menu), but the action is dropped.
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        let _ = decide_press(12, FLAG_COMMAND as u32, false, false);
        publish(PanelSnapshot::new(true, false, PanelKeymap::default()));
        assert_eq!(
            take_pending_action(12),
            PendingOutcome::Cancelled,
            "a switch turned off cancels the action but the event is still consumed"
        );
        clear_claims();
    }

    #[test]
    fn a_hard_stop_drops_the_held_record() {
        // The tap stopping means any outstanding key-up can no longer be received. Keeping the held
        // record would make the next real press of that key look like a repeat and be swallowed --
        // a legitimate shortcut going dead after a permission blip.
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        assert_eq!(
            decide_press(13, FLAG_COMMAND as u32, false, false),
            ClaimDecision::PassThrough
        );
        clear_claims();
        // After the hard stop the same key must be able to establish a fresh claim.
        assert_eq!(
            decide_press(13, FLAG_COMMAND as u32, false, false),
            ClaimDecision::PassThrough,
            "a key must not be stuck looking held after the tap stopped"
        );
        clear_claims();
    }

    #[test]
    fn an_unowned_autorepeat_never_creates_a_claim() {
        let _guard = state_lock();
        clear_claims();
        publish(snapshot_visible(false));
        assert_eq!(
            decide_press(12, FLAG_COMMAND as u32, false, true),
            ClaimDecision::NotClaimed,
            "a repeat with no hold behind it must not start one"
        );
        assert!(!has_pending_action(12));
        clear_claims();
    }
}
