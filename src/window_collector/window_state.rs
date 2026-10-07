//! Pure decode of one window's state evidence into the flags a card presents.
//!
//! Two evidence planes feed this module and they are deliberately not interchangeable:
//!
//! - the **WindowServer row** ([`WsWindowRow`]) owns physical state: ordered-in, the minimized tag,
//!   and which kind of Space the window's Space is. It is readable for every window this process
//!   can name, including windows whose app publishes no accessibility element for them (a window on
//!   another desktop leaves `kAXWindows` because AppKit builds that list from a Space-restricted
//!   WindowServer query).
//! - the **AX reads** own what only the window's own app knows, and they are a call into that app.
//!
//! Every verdict therefore carries a [`StateSource`]: which plane actually decided it. `Unknown`
//! means nothing in this pass decided it, so the caller keeps its previous fallback value; it is
//! never reported as though the WindowServer had confirmed something.
//!
//! The bit positions in here are undocumented WindowServer fields. Measured on this machine
//! (macOS 27.0.1 / 26A434, 2026-10-07) with `--space-state-record`, which walks the app's own probe
//! window and prints the raw fields: `attributes` bit `0x2` (ordered in), `tags` bit 60 (minimized)
//! and bit 39 (app hidden) all reproduced the reference implementation's state matrix, and
//! `--smoke-space-state-matrix` now asserts the recorded cells. The one field NOT verified locally
//! is the fullscreen Space mask `0x20`: this app is a menu-bar application and AppKit refuses
//! `toggleFullScreen:` for it, so the probe cannot produce a fullscreen window (the walk records
//! why). `0x20` is the reference implementation's measured value on macOS 26 and is covered here
//! only by unit tests until a real fullscreen window is recorded. Re-measure after a major macOS
//! release instead of trusting the numbers in these comments.

use crate::skylight::WsWindowRow;

/// `attributes` bit set while the window is ordered in (on screen). It is cleared by minimize,
/// app-hide, moving the window to another Space, and by a closing window mid-teardown, so it is an
/// on-screen signal and NOT a minimized signal.
const ATTRIBUTE_ORDERED_IN: u64 = 0x2;
/// `tags` bit set exactly while the window is minimized.
const TAG_MINIMIZED: u64 = 1 << 60;
/// `tags` bit set while the window's application is hidden (Command+H). Published for
/// cross-checking only; the presented hidden state still comes from AppKit.
const TAG_APP_HIDDEN: u64 = 1 << 39;
/// `space_type_mask` bit set while the window's Space is a fullscreen Space.
const MASK_FULLSCREEN_SPACE: u64 = 0x20;
/// `space_type_mask` bit set while the window's Space is an ordinary desktop.
const MASK_ORDINARY_SPACE: u64 = 0x1;

/// Which evidence decided a decoded value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum StateSource {
    /// Nothing decided it in this pass: the caller's fallback value stands.
    #[default]
    Unknown,
    /// An accessibility read from the window's own application.
    Ax,
    /// A WindowServer row field.
    WindowServer,
    /// Geometry only (bounds matching a display rect). Speculation, not a WindowServer fact.
    Geometry,
    /// AppKit, which is where the hidden state still comes from.
    AppKit,
}

/// The minimized evidence AX offers for one window.
///
/// `AxWindowInfo::minimized` is a plain `bool` (a failed read falls back to `false`), so AX can
/// report a value or report nothing at all; there is no third state to distinguish here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AxMinimized {
    /// AX published no element for this window.
    NoElement,
    /// AX paired this window and reported this value.
    Known(bool),
}

/// The fullscreen evidence AX offers for one window. `Paired(None)` is "AX has an element but the
/// attribute did not answer", which must not be confused with "there is no AX element at all":
/// the first keeps today's bounds fallback, the second is the other-desktop shape where a valid
/// ordinary-Space mask outranks geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AxFullscreen {
    NoElement,
    Paired(Option<bool>),
}

/// Which ordinary/fullscreen kind the WindowServer reported for a window's Space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SpaceMaskKind {
    Ordinary,
    Fullscreen,
}

/// All the diagnostics the collector publishes for one card. It travels with the card: the rows are
/// the ones the pass that produced the card read, so a stale pass can never explain a newer card.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct WindowStateEvidence {
    /// Ordered-in as the WindowServer reported it (`None` = the field could not be read).
    pub(crate) ordered_in: Option<bool>,
    pub(crate) minimized_source: StateSource,
    pub(crate) fullscreen_source: StateSource,
    pub(crate) app_hidden_source: StateSource,
    /// Which AX route paired this window in the pass that produced the card. `None` = not recorded
    /// (test fixtures); production always records it.
    pub(crate) pairing: Option<super::collect::WindowPairingSource>,
    /// The raw row this card was decoded from, for `--e2e-state` only.
    pub(crate) row: Option<WsWindowRow>,
}

pub(crate) fn ordered_in(row: Option<&WsWindowRow>) -> Option<bool> {
    row.and_then(|row| row.attributes)
        .map(|attributes| attributes & ATTRIBUTE_ORDERED_IN != 0)
}

fn minimized_tag(row: Option<&WsWindowRow>) -> Option<bool> {
    row.and_then(|row| row.tags)
        .map(|tags| tags & TAG_MINIMIZED != 0)
}

/// The Space kind the WindowServer reported for this window (`None` = no decisive kind: either the
/// mask was unavailable or it carried neither bit).
pub(crate) fn space_mask_kind(row: Option<&WsWindowRow>) -> Option<SpaceMaskKind> {
    let mask = row.and_then(|row| row.space_type_mask)?;
    if mask & MASK_FULLSCREEN_SPACE != 0 {
        Some(SpaceMaskKind::Fullscreen)
    } else if mask & MASK_ORDINARY_SPACE != 0 {
        Some(SpaceMaskKind::Ordinary)
    } else {
        None
    }
}

/// Whether the app published an accessibility element for this window; exposed so `--e2e-state` can
/// prove a card came through the no-element route without consulting the AX identity history (which
/// answers "AX has named this window at some point", a different question).
pub(crate) fn ax_app_hidden_tag(row: Option<&WsWindowRow>) -> Option<bool> {
    row.and_then(|row| row.tags)
        .map(|tags| tags & TAG_APP_HIDDEN != 0)
}

/// Minimized, with the evidence precedence the evidence requires.
///
/// The restore case is why the ordered-in bit outranks the other two. Two separate observations
/// justify it, and they are not the same experiment:
///
/// - the reference implementation measured a *Dock* restore, where the WindowServer keeps its
///   minimized tag set for a while after the accessibility read already says "restored" (their
///   figure: ~644ms);
/// - this repository's `--space-state-record` run used `deminiaturize:` (a programmatic restore, not
///   a Dock click) and saw `attributes` bit `0x2` come back with the tag already clear in the same
///   20ms-polled sample -- it did NOT reproduce a delayed clear, and it cannot speak for the Dock
///   path.
///
/// Either way the conservative rule is the same, and it is the one that cannot be wrong on the
/// screen the user is looking at: while the WindowServer says the window is ordered in, it is not
/// presented as minimized.
///
/// When the ordered-in field could not be read, AX decides on its own and the WindowServer tag is
/// only consulted for a window AX does not publish at all — an "AX says false, tag says true" pair
/// must not turn into a minimized card on the strength of a field that was not read.
pub(crate) fn decode_minimized(row: Option<&WsWindowRow>, ax: AxMinimized) -> (bool, StateSource) {
    match ordered_in(row) {
        Some(true) => (false, StateSource::WindowServer),
        Some(false) => match ax {
            AxMinimized::Known(true) => (true, StateSource::Ax),
            AxMinimized::Known(false) => match minimized_tag(row) {
                Some(true) => (true, StateSource::WindowServer),
                Some(false) => (false, StateSource::Ax),
                None => (false, StateSource::Ax),
            },
            AxMinimized::NoElement => match minimized_tag(row) {
                Some(value) => (value, StateSource::WindowServer),
                None => (false, StateSource::Unknown),
            },
        },
        None => match ax {
            AxMinimized::Known(value) => (value, StateSource::Ax),
            AxMinimized::NoElement => match minimized_tag(row) {
                Some(value) => (value, StateSource::WindowServer),
                None => (false, StateSource::Unknown),
            },
        },
    }
}

/// Native fullscreen.
///
/// `bounds_is_fullscreen` is the caller's geometry verdict (bounds matching a display rect). It is
/// speculation: a display-sized ordinary window looks identical to a fullscreen one, which is why
/// the WindowServer's Space kind suppresses it wherever it is available.
///
/// A window that has an AX element keeps today's behavior exactly (`ax || bounds`); the Space mask
/// only adds a positive fullscreen source there. The mask decides on its own only for a window with
/// no AX element, which is the other-desktop case this work exists for.
pub(crate) fn decode_fullscreen(
    ax: AxFullscreen,
    row: Option<&WsWindowRow>,
    bounds_is_fullscreen: bool,
) -> (bool, StateSource) {
    match ax {
        AxFullscreen::Paired(Some(true)) => (true, StateSource::Ax),
        AxFullscreen::Paired(attribute) => {
            if bounds_is_fullscreen {
                (true, StateSource::Geometry)
            } else {
                match space_mask_kind(row) {
                    Some(SpaceMaskKind::Fullscreen) => (true, StateSource::WindowServer),
                    Some(SpaceMaskKind::Ordinary) => (false, StateSource::WindowServer),
                    None => match attribute {
                        Some(false) => (false, StateSource::Ax),
                        _ => (false, StateSource::Unknown),
                    },
                }
            }
        }
        AxFullscreen::NoElement => match space_mask_kind(row) {
            Some(SpaceMaskKind::Fullscreen) => (true, StateSource::WindowServer),
            Some(SpaceMaskKind::Ordinary) => (false, StateSource::WindowServer),
            None => {
                if bounds_is_fullscreen {
                    (true, StateSource::Geometry)
                } else {
                    (false, StateSource::Unknown)
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skylight::WsRowCapabilities;

    fn row(attributes: Option<u64>, tags: Option<u64>, mask: Option<u64>) -> WsWindowRow {
        WsWindowRow {
            window_id: 100,
            attributes,
            tags,
            space_type_mask: mask,
            ..WsWindowRow::default()
        }
    }

    const ORDERED_IN: u64 = 0x3;
    const ORDERED_OUT: u64 = 0x1;
    const MINIMIZED_TAG: u64 = 1 << 60;
    const HIDDEN_TAG: u64 = 1 << 39;
    const FULLSCREEN_MASK: u64 = 0x20;
    const ORDINARY_MASK: u64 = 0x1;

    #[test]
    fn a_missing_new_getter_never_clears_an_older_capability() {
        // The fields are decided by their own symbols alone; this is the policy the row query and
        // the two legacy wrappers rely on.
        let capabilities = WsRowCapabilities::from_symbols(false, false, false, true, true);
        assert!(capabilities.space_type_mask);
        assert!(capabilities.parent_id);
        assert!(!capabilities.attributes);
        assert!(!capabilities.tags);
        assert!(!capabilities.pid);
    }

    #[test]
    fn ordered_in_comes_from_the_attribute_bit_only() {
        assert_eq!(
            ordered_in(Some(&row(Some(ORDERED_IN), None, None))),
            Some(true)
        );
        assert_eq!(
            ordered_in(Some(&row(Some(ORDERED_OUT), None, None))),
            Some(false)
        );
        assert_eq!(
            ordered_in(Some(&row(None, Some(MINIMIZED_TAG), None))),
            None
        );
        assert_eq!(ordered_in(None), None);
    }

    #[test]
    fn a_window_back_on_screen_is_not_minimized_even_with_a_stale_tag() {
        // A restore in progress (the reference implementation measured the Dock path, where the tag
        // outlives the order-in): the ordered-in bit decides, so the stale tag cannot present a
        // window that is already back on screen as minimized.
        assert_eq!(
            decode_minimized(
                Some(&row(Some(ORDERED_IN), Some(MINIMIZED_TAG), None)),
                AxMinimized::Known(false)
            ),
            (false, StateSource::WindowServer)
        );
        // The same window with AX still saying minimized: on screen wins.
        assert_eq!(
            decode_minimized(
                Some(&row(Some(ORDERED_IN), Some(MINIMIZED_TAG), None)),
                AxMinimized::Known(true)
            ),
            (false, StateSource::WindowServer)
        );
    }

    #[test]
    fn a_window_ordered_out_believes_either_source() {
        assert_eq!(
            decode_minimized(
                Some(&row(Some(ORDERED_OUT), Some(MINIMIZED_TAG), None)),
                AxMinimized::NoElement
            ),
            (true, StateSource::WindowServer)
        );
        assert_eq!(
            decode_minimized(
                Some(&row(Some(ORDERED_OUT), None, None)),
                AxMinimized::Known(true)
            ),
            (true, StateSource::Ax)
        );
        // Ordered out with no minimized claim is an orderOut'd or background-tab window, not a
        // minimized one.
        assert_eq!(
            decode_minimized(
                Some(&row(Some(ORDERED_OUT), Some(0), None)),
                AxMinimized::Known(false)
            ),
            (false, StateSource::Ax)
        );
    }

    #[test]
    fn a_missing_ordered_in_field_lets_ax_decide_alone() {
        // The counter-example that killed the naive OR: the tag has not cleared yet, AX already
        // says the window is restored, and the ordered-in field is unavailable.
        assert_eq!(
            decode_minimized(
                Some(&row(None, Some(MINIMIZED_TAG), None)),
                AxMinimized::Known(false)
            ),
            (false, StateSource::Ax)
        );
        assert_eq!(
            decode_minimized(Some(&row(None, Some(0), None)), AxMinimized::Known(true)),
            (true, StateSource::Ax)
        );
    }

    #[test]
    fn the_tag_decides_only_when_ax_publishes_nothing() {
        assert_eq!(
            decode_minimized(
                Some(&row(None, Some(MINIMIZED_TAG), None)),
                AxMinimized::NoElement
            ),
            (true, StateSource::WindowServer)
        );
        // No tag either: nothing decided it, so the source says so instead of claiming the
        // WindowServer confirmed "not minimized".
        assert_eq!(
            decode_minimized(
                Some(&row(Some(ORDERED_OUT), None, None)),
                AxMinimized::NoElement
            ),
            (false, StateSource::Unknown)
        );
        assert_eq!(
            decode_minimized(None, AxMinimized::NoElement),
            (false, StateSource::Unknown)
        );
    }

    #[test]
    fn fullscreen_keeps_the_accessibility_path_unchanged() {
        // An AX window keeps today's `ax || bounds`; the Space mask only adds a positive source.
        assert_eq!(
            decode_fullscreen(AxFullscreen::Paired(Some(true)), None, false),
            (true, StateSource::Ax)
        );
        assert_eq!(
            decode_fullscreen(AxFullscreen::Paired(Some(false)), None, true),
            (true, StateSource::Geometry)
        );
        assert_eq!(
            decode_fullscreen(
                AxFullscreen::Paired(Some(false)),
                Some(&row(Some(ORDERED_IN), None, Some(ORDINARY_MASK))),
                true
            ),
            (true, StateSource::Geometry)
        );
        assert_eq!(
            decode_fullscreen(
                AxFullscreen::Paired(Some(false)),
                Some(&row(Some(ORDERED_IN), None, Some(FULLSCREEN_MASK))),
                false
            ),
            (true, StateSource::WindowServer)
        );
    }

    #[test]
    fn an_accessibility_window_with_an_unread_attribute_is_not_a_confirmed_false() {
        assert_eq!(
            decode_fullscreen(AxFullscreen::Paired(None), None, false),
            (false, StateSource::Unknown)
        );
        assert_eq!(
            decode_fullscreen(AxFullscreen::Paired(Some(false)), None, false),
            (false, StateSource::Ax)
        );
    }

    #[test]
    fn an_ordinary_space_outranks_the_geometry_guess_without_an_ax_element() {
        // A window sized to the display on an ordinary Space is not fullscreen.
        assert_eq!(
            decode_fullscreen(
                AxFullscreen::NoElement,
                Some(&row(Some(ORDERED_IN), None, Some(ORDINARY_MASK))),
                true
            ),
            (false, StateSource::WindowServer)
        );
        assert_eq!(
            decode_fullscreen(
                AxFullscreen::NoElement,
                Some(&row(Some(ORDERED_IN), None, Some(FULLSCREEN_MASK))),
                false
            ),
            (true, StateSource::WindowServer)
        );
        // No mask at all: geometry is all that is left, and it says so.
        assert_eq!(
            decode_fullscreen(AxFullscreen::NoElement, None, true),
            (true, StateSource::Geometry)
        );
        assert_eq!(
            decode_fullscreen(AxFullscreen::NoElement, None, false),
            (false, StateSource::Unknown)
        );
    }

    #[test]
    fn the_space_mask_kind_ignores_a_mask_with_neither_bit() {
        assert_eq!(
            space_mask_kind(Some(&row(None, None, Some(0x4)))),
            None,
            "an unrecognized mask is not evidence of an ordinary Space"
        );
        assert_eq!(
            space_mask_kind(Some(&row(None, None, Some(ORDINARY_MASK)))),
            Some(SpaceMaskKind::Ordinary)
        );
        assert_eq!(
            space_mask_kind(Some(&row(None, None, Some(FULLSCREEN_MASK)))),
            Some(SpaceMaskKind::Fullscreen)
        );
        assert_eq!(space_mask_kind(None), None);
    }

    #[test]
    fn the_hidden_tag_is_published_but_decides_nothing_yet() {
        assert_eq!(
            ax_app_hidden_tag(Some(&row(None, Some(HIDDEN_TAG), None))),
            Some(true)
        );
        assert_eq!(
            ax_app_hidden_tag(Some(&row(None, Some(0), None))),
            Some(false)
        );
        assert_eq!(ax_app_hidden_tag(None), None);
    }
}
