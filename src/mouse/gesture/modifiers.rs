//! Modifier-key ownership: who currently holds which modifier.
//!
//! The mouse button mappings and the gesture transactions both synthesise modifier presses, and the
//! user may be holding the same modifier physically. Each of them used to assume it was the only
//! holder: \`keysim::release_up\` walked the flags back to zero, which lifts a key another holder still
//! needs. This arbiter makes the holder set explicit, and -- more importantly -- makes the *forwarded*
//! event agree with it: a physical key event is rewritten at the tap so the application never sees a
//! modifier released while a synthetic holder still needs it.
//!
//! Scope limit: only the four modifier *categories* are managed. Left and right variants of one
//! category are not distinguished, which matches every shortcut the config grammar can express.

/// Modifier bits, matching CGEventFlags / NSEvent.
pub(crate) const FLAG_COMMAND: u32 = 0x0010_0000;
pub(crate) const FLAG_OPTION: u32 = 0x0008_0000;
pub(crate) const FLAG_CONTROL: u32 = 0x0004_0000;
pub(crate) const FLAG_SHIFT: u32 = 0x0002_0000;

/// The bits this module rewrites. Everything else in \`flags\` (Caps Lock, Fn, numeric pad, ...) is
/// passed through untouched.
pub(crate) const MANAGED_MASK: u32 = FLAG_COMMAND | FLAG_OPTION | FLAG_CONTROL | FLAG_SHIFT;

/// Who is holding a modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum HolderId {
    /// A mouse button mapping (the button number that started the transaction).
    Button(u32),
    /// One gesture press.
    Gesture(u64),
    /// The physical keyboard: the user is holding the key themselves. Not registered as a holder:
    /// the physical state lives in \`physical\` and is merged separately, so the set never needs it.
    #[cfg(test)]
    Physical,
}

/// The pure merge rule, extracted so the test can assert the forwarded value directly.
///
/// \`physical\` is the state the physical event carried; \`synthetic\` is what our own holders require.
/// A bit required by either side is present in the output, so a physical release never removes a
/// modifier a synthetic holder still needs, and a synthetic release never removes one the user is
/// physically holding.
pub(crate) fn merge_flags(event_flags: u32, physical: u32, synthetic: u32) -> u32 {
    (event_flags & !MANAGED_MASK) | (physical | synthetic)
}

#[derive(Debug, Default)]
pub(crate) struct ModifierOwnership {
    /// Physical modifier state, maintained from the *pre-rewrite* event flags.
    physical: u32,
    /// Per-bit holder sets. A bit with an empty set is not asserted.
    holders: std::collections::BTreeMap<u32, std::collections::BTreeSet<HolderId>>,
}

impl ModifierOwnership {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Seed the physical state once, at startup. \`CGEventSourceFlagsState\` is the only source used
    /// here; afterwards the physical flags of each event keep it current, so a key released later is
    /// not resurrected by this initial value.
    pub(crate) fn seed_physical(&mut self, flags: u32) {
        self.physical = flags & MANAGED_MASK;
    }

    /// The bits our holders require.
    pub(crate) fn synthetic(&self) -> u32 {
        self.holders
            .iter()
            .filter(|(_, holders)| !holders.is_empty())
            .fold(0, |acc, (bit, _)| acc | *bit)
    }

    /// A physical modifier event arrived. Updates the physical state from the *pre-rewrite* flags.
    ///
    /// The physical event itself is what the application receives (rewritten by [[forward]]), so this
    /// never needs to publish a synthetic flagsChanged of its own.
    pub(crate) fn observe_physical(&mut self, flags: u32) {
        self.physical = flags & MANAGED_MASK;
    }

    /// The merged state an event would carry right now. Used to decide whether a holder change is
    /// visible to the application at all: a synthetic bit the user is already holding physically
    /// needs no flagsChanged, because the forwarded event already carries it.
    fn merged(&self) -> u32 {
        self.synthetic() | self.physical
    }

    /// Register \`holder\` for the bits in \`flags\`. Returns the flagsChanged value the caller should
    /// publish **if the merged state changed**; \`None\` when the application cannot tell the
    /// difference and no event is needed.
    pub(crate) fn acquire(&mut self, holder: HolderId, flags: u32) -> Option<u32> {
        let before = self.merged();
        let mut changed = false;
        for bit in [FLAG_COMMAND, FLAG_OPTION, FLAG_CONTROL, FLAG_SHIFT] {
            if flags & bit != 0 {
                let set = self.holders.entry(bit).or_default();
                if set.insert(holder) {
                    changed = true;
                }
            }
        }
        if !changed {
            return None;
        }
        let after = self.merged();
        (before != after).then_some(self.merged_event_flags(after))
    }

    /// Release \`holder\`. Same return contract as [[acquire]]: an event only when the application
    /// would see a different modifier state.
    pub(crate) fn release(&mut self, holder: HolderId, flags: u32) -> Option<u32> {
        let before = self.merged();
        let mut changed = false;
        for bit in [FLAG_COMMAND, FLAG_OPTION, FLAG_CONTROL, FLAG_SHIFT] {
            if flags & bit != 0 {
                if let Some(set) = self.holders.get_mut(&bit) {
                    if set.remove(&holder) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            return None;
        }
        let after = self.merged();
        (before != after).then_some(self.merged_event_flags(after))
    }

    /// The flags to carry on a published flagsChanged: the merged state with every non-managed bit
    /// cleared (those belong to the physical event that produced the change).
    fn merged_event_flags(&self, synthetic: u32) -> u32 {
        synthetic
    }

    /// The flags to forward for an arbitrary event: its own non-managed bits plus the merged state.
    pub(crate) fn forward(&self, event_flags: u32) -> u32 {
        merge_flags(event_flags, self.physical, self.synthetic())
    }

    /// Holders currently registered for \`bit\`, for diagnostics and tests.
    #[cfg(test)]
    pub(crate) fn holders_of(&self, bit: u32) -> Vec<HolderId> {
        self.holders
            .get(&bit)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn physical(&self) -> u32 {
        self.physical
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: u32 = FLAG_COMMAND;

    #[test]
    fn the_merge_is_identity_without_synthetic_holders() {
        for flags in [0u32, CMD, FLAG_SHIFT, CMD | FLAG_SHIFT, 0x0001_0000 | CMD] {
            assert_eq!(merge_flags(flags, flags & MANAGED_MASK, 0), flags);
        }
    }

    #[test]
    fn a_physical_release_keeps_a_synthetic_holder() {
        // The reviewer's counter-example: the user lets go of Cmd while a button mapping still
        // synthesises it. The forwarded event must keep Cmd down.
        let mut arb = ModifierOwnership::new();
        arb.seed_physical(CMD);
        // The application already sees Cmd from the physical key: no synthetic event is needed.
        assert_eq!(arb.acquire(HolderId::Button(3), CMD), None);
        arb.observe_physical(0); // the user released Cmd
        assert_eq!(arb.forward(0), CMD, "forwarded event keeps Cmd");
    }

    #[test]
    fn the_last_synthetic_holder_releases_the_key() {
        let mut arb = ModifierOwnership::new();
        arb.seed_physical(0);
        assert_eq!(arb.acquire(HolderId::Gesture(1), CMD), Some(CMD));
        assert_eq!(
            arb.acquire(HolderId::Button(3), CMD),
            None,
            "no extra event"
        );
        assert_eq!(arb.release(HolderId::Gesture(1), CMD), None, "still held");
        assert_eq!(arb.release(HolderId::Button(3), CMD), Some(0));
        assert_eq!(arb.forward(0), 0);
    }

    #[test]
    fn a_physical_press_survives_a_synthetic_release() {
        // The opposite order: the user holds Cmd physically and a gesture transaction ends.
        let mut arb = ModifierOwnership::new();
        arb.seed_physical(0);
        arb.acquire(HolderId::Gesture(1), CMD);
        arb.observe_physical(CMD); // the user presses Cmd too
        assert_eq!(
            arb.release(HolderId::Gesture(1), CMD),
            None,
            "Cmd is still down"
        );
        assert_eq!(arb.forward(CMD), CMD);
    }

    #[test]
    fn seeding_does_not_resurrect_a_released_key() {
        // The reviewer's counter-example for the baseline: Cmd was down at startup, then released.
        let mut arb = ModifierOwnership::new();
        arb.seed_physical(CMD);
        arb.observe_physical(0);
        assert_eq!(arb.physical(), 0);
        assert_eq!(arb.forward(0), 0);
    }

    #[test]
    fn no_synthetic_holder_no_flags_changed() {
        let mut arb = ModifierOwnership::new();
        assert_eq!(arb.acquire(HolderId::Gesture(1), 0), None);
        assert_eq!(arb.release(HolderId::Gesture(1), 0), None);
    }

    #[test]
    fn releasing_one_holder_does_not_clear_anothers_bit() {
        let mut arb = ModifierOwnership::new();
        arb.acquire(HolderId::Gesture(1), CMD);
        arb.acquire(HolderId::Button(3), FLAG_SHIFT);
        assert_eq!(arb.release(HolderId::Gesture(1), CMD), Some(FLAG_SHIFT));
        assert_eq!(arb.holders_of(FLAG_SHIFT), vec![HolderId::Button(3)]);
    }
}
