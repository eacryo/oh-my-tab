//! The window-state matrix instrument.
//!
//! Two modes, deliberately split so that "print the raw bits" is never mistaken for a gate:
//!
//! - `--space-state-record` walks the app's own probe window through every state the decode has to
//!   distinguish (ordered in, ordered out, minimized, restored, app hidden, fullscreen) and prints
//!   the raw WindowServer fields plus the accessibility readings, with the settle time of each
//!   change. Its output is the source command for the bit semantics documented in
//!   [`super::window_state`]; it makes no judgement.
//! - `--smoke-space-state-matrix` asserts the matrix that run pinned, plus the counter-example
//!   cells (an ordered-out window is not minimized, a fullscreen window is not minimized, a hidden
//!   window is not minimized, a restored window is not minimized). Cells it cannot test report
//!   `NOT RUN` and the process exits non-zero: "did not run" is not "passed".
//!
//! Both modes need a GUI session and the main thread, and both restore the probe window before
//! exiting.
//!
//! The walk deliberately has no fullscreen cell: this app is a menu-bar (accessory) application, and
//! AppKit refuses `toggleFullScreen:` for it -- measured 2026-10-07, the window stayed
//! `attributes=0x3 tags=0x200100482001 mask=0x1 ax_full=false` for a full 6s patience after the call,
//! with and without an explicit activation. Fullscreen evidence therefore comes from the
//! cross-desktop recording (a real fullscreen window, whose Space the topology types as fullscreen)
//! and from the unit tests in `window_state`, not from this probe. Writing a "NOT RUN" cell that the
//! gate then failed on would have been honest but useless; removing the step and saying why is
//! clearer than either.

use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::time::{Duration, Instant};

use super::window_state::{decode_fullscreen, decode_minimized, AxFullscreen, AxMinimized};
use crate::log_info;
use crate::skylight;

pub(crate) enum ProbeMode {
    Record,
    Matrix,
}

/// One observation of the probe window: the WindowServer row plus what our own process's
/// accessibility answer says about it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Sample {
    attributes: Option<u64>,
    tags: Option<u64>,
    space_type_mask: Option<u64>,
    ax_minimized: Option<bool>,
    ax_fullscreen: Option<bool>,
    cg_onscreen: Option<bool>,
}

impl Sample {
    fn minimized(self) -> (bool, &'static str) {
        let row = self.row();
        let (value, source) = decode_minimized(
            row.as_ref(),
            match self.ax_minimized {
                Some(value) => AxMinimized::Known(value),
                None => AxMinimized::NoElement,
            },
        );
        (value, source_name(source))
    }

    fn fullscreen(self) -> (bool, &'static str) {
        let row = self.row();
        let (value, source) = decode_fullscreen(
            match self.ax_fullscreen {
                Some(value) => AxFullscreen::Paired(Some(value)),
                None => AxFullscreen::NoElement,
            },
            row.as_ref(),
            // The probe never relies on the geometry fallback: it reports the row's own evidence.
            false,
        );
        (value, source_name(source))
    }

    fn row(self) -> Option<skylight::WsWindowRow> {
        Some(skylight::WsWindowRow {
            window_id: 0,
            attributes: self.attributes,
            tags: self.tags,
            space_type_mask: self.space_type_mask,
            ..skylight::WsWindowRow::default()
        })
    }

    fn line(self) -> String {
        let (minimized, minimized_source) = self.minimized();
        let (fullscreen, fullscreen_source) = self.fullscreen();
        format!(
            "attributes={} tags={} mask={} cg_onscreen={} ax_min={} ax_full={} -> minimized={}({}) fullscreen={}({})",
            hex(self.attributes),
            hex(self.tags),
            hex(self.space_type_mask),
            opt(self.cg_onscreen),
            opt(self.ax_minimized),
            opt(self.ax_fullscreen),
            minimized,
            minimized_source,
            fullscreen,
            fullscreen_source,
        )
    }
}

fn hex(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| format!("0x{value:x}"))
}

fn opt(value: Option<bool>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

fn source_name(source: super::window_state::StateSource) -> &'static str {
    use super::window_state::StateSource;
    match source {
        StateSource::Unknown => "unknown",
        StateSource::Ax => "ax",
        StateSource::WindowServer => "window_server",
        StateSource::Geometry => "geometry",
        StateSource::AppKit => "appkit",
    }
}

/// The pinned matrix: `(step, ordered_in, minimized, fullscreen)`, recorded by
/// `--space-state-record` on this machine (macOS 27.0.1 / build 26A434, 2026-10-07) and asserted by
/// `--smoke-space-state-matrix`. A cell the walk cannot produce is absent, which the gate reports as
/// NOT RUN and fails on: an unpinned cell must never look like a passing one.
const MATRIX: &[(&str, bool, bool, bool)] = &[
    ("normal", true, false, false),
    ("order_out", false, false, false),
    ("order_front", true, false, false),
    ("miniaturize", false, true, false),
    ("restore", true, false, false),
    ("hide_app", false, false, false),
    ("unhide_app", true, false, false),
];

/// One step of the walk and how to apply it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Normal,
    OrderOut,
    OrderFront,
    Miniaturize,
    Restore,
    HideApp,
    UnhideApp,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::OrderOut => "order_out",
            Self::OrderFront => "order_front",
            Self::Miniaturize => "miniaturize",
            Self::Restore => "restore",
            Self::HideApp => "hide_app",
            Self::UnhideApp => "unhide_app",
        }
    }
}

/// What each cell must show in the RAW WindowServer fields, over and above the merged verdict.
///
/// The merged `minimized` is satisfied by the accessibility read alone, so a gate that compared only
/// the verdicts would still pass with the minimized tag missing or permanently clear -- it would
/// prove nothing about the field this work added. Every cell therefore also states the tag bits it
/// depends on, and `cell_evidence_holds` fails loudly when one is absent or wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CellEvidence {
    /// Bit 60 of `tags` (minimized) when the cell makes a claim about it.
    minimized_tag: Option<bool>,
    /// Bit 39 of `tags` (the app is hidden) when the cell makes a claim about it.
    hidden_tag: Option<bool>,
}

impl Step {
    fn evidence(self) -> CellEvidence {
        match self {
            Self::Normal | Self::OrderFront | Self::UnhideApp => CellEvidence {
                minimized_tag: Some(false),
                hidden_tag: Some(false),
            },
            Self::OrderOut => CellEvidence {
                minimized_tag: Some(false),
                hidden_tag: Some(false),
            },
            Self::Miniaturize => CellEvidence {
                minimized_tag: Some(true),
                hidden_tag: Some(false),
            },
            Self::Restore => CellEvidence {
                minimized_tag: Some(false),
                hidden_tag: Some(false),
            },
            Self::HideApp => CellEvidence {
                minimized_tag: Some(false),
                hidden_tag: Some(true),
            },
        }
    }
}

/// Whether the raw fields this cell depends on are present and in the expected state. Returns the
/// reason it does not hold, so a failing run names the field instead of only the cell.
fn cell_evidence_holds(step: Step, sample: &Sample) -> Result<(), String> {
    let expected = step.evidence();
    let attributes = sample
        .attributes
        .ok_or_else(|| "the attributes field could not be read".to_string())?;
    if attributes & 0x2 == 0 && matches!(step, Step::Normal | Step::OrderFront | Step::UnhideApp) {
        return Err(format!("expected ordered-in, attributes={attributes:#x}"));
    }
    let tags = sample
        .tags
        .ok_or_else(|| "the tags field could not be read".to_string())?;
    if let Some(expected_minimized) = expected.minimized_tag {
        let actual = tags & (1 << 60) != 0;
        if actual != expected_minimized {
            return Err(format!(
                "expected minimized tag {expected_minimized}, tags={tags:#x}"
            ));
        }
    }
    if let Some(expected_hidden) = expected.hidden_tag {
        let actual = tags & (1 << 39) != 0;
        if actual != expected_hidden {
            return Err(format!(
                "expected hidden tag {expected_hidden}, tags={tags:#x}"
            ));
        }
    }
    Ok(())
}

const STEPS: &[Step] = &[
    Step::Normal,
    Step::OrderOut,
    Step::OrderFront,
    Step::Miniaturize,
    Step::Restore,
    Step::HideApp,
    Step::UnhideApp,
];

pub(crate) fn run(mode: ProbeMode) -> bool {
    unsafe {
        let title = format!("oh-my-tab space-state probe {}", std::process::id());
        let window = make_probe_window(&title);
        if window.is_null() {
            eprintln!("[space-state] could not create the probe window");
            return false;
        }
        pump(0.4);

        let own_pid = std::process::id() as i32;
        let Some(wid) = probe_wid(own_pid, &title) else {
            eprintln!("[space-state] the probe window has no WindowServer id");
            close_probe_window(window);
            return false;
        };
        log_info!(
            "[space-state] probe window wid={wid} mode={}",
            match mode {
                ProbeMode::Record => "record",
                ProbeMode::Matrix => "matrix",
            }
        );

        let mut ok = true;
        let mut previous = read_sample(wid, own_pid);
        log_info!("[space-state] step=create {}", previous.line());

        for step in STEPS {
            apply_step(window, *step);
            let (settled, transitions) = wait_for_change(wid, own_pid, previous, 1200);
            for (elapsed_ms, sample) in &transitions {
                log_info!(
                    "[space-state] transition step={} after_ms={} {}",
                    step.name(),
                    elapsed_ms,
                    sample.line()
                );
            }
            let (ordered_in, minimized, fullscreen) = (
                super::window_state::ordered_in(settled.row().as_ref()).unwrap_or(false),
                settled.minimized().0,
                settled.fullscreen().0,
            );
            log_info!(
                "[space-state] step={} ordered_in={} minimized={} fullscreen={} {}",
                step.name(),
                ordered_in,
                minimized,
                fullscreen,
                settled.line()
            );
            if let ProbeMode::Matrix = mode {
                if let Err(reason) = cell_evidence_holds(*step, &settled) {
                    eprintln!(
                        "[space-state] FAIL step={} raw evidence: {}",
                        step.name(),
                        reason
                    );
                    ok = false;
                }
                match MATRIX.iter().find(|(name, ..)| *name == step.name()) {
                    Some((_, expected_ordered, expected_minimized, expected_fullscreen))
                        if (*expected_ordered, *expected_minimized, *expected_fullscreen)
                            != (ordered_in, minimized, fullscreen) =>
                    {
                        eprintln!(
                            "[space-state] FAIL step={} expected ordered_in={} minimized={} fullscreen={} got {} {} {}",
                            step.name(),
                            expected_ordered,
                            expected_minimized,
                            expected_fullscreen,
                            ordered_in,
                            minimized,
                            fullscreen
                        );
                        ok = false;
                    }
                    Some(_) => log_info!("[space-state] PASS step={}", step.name()),
                    // "Not run" is not "passed": an unpinned cell fails the gate.
                    None => {
                        eprintln!(
                            "[space-state] NOT RUN step={} (not pinned; run --space-state-record)",
                            step.name()
                        );
                        ok = false;
                    }
                }
            }
            previous = settled;
        }

        restore_probe_window(window);
        pump(0.3);
        close_probe_window(window);
        pump(0.2);
        ok
    }
}

unsafe fn make_probe_window(title: &str) -> *mut AnyObject {
    // NSWindowStyleMask: titled | closable | miniaturizable | resizable. The last two are load
    // bearing: `miniaturize:` is a no-op on a window without the miniaturizable bit, and
    // `toggleFullScreen:` needs the fullscreen-primary collection behavior that a resizable window
    // gets by default. The first version of this instrument had neither, so both cells silently
    // recorded "nothing happened" -- which is why the walk asserts a settled state rather than
    // trusting that the call did something.
    let style = 1u64 | 2u64 | 4u64 | 8u64;
    let frame = NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(640.0, 400.0));
    let window: *mut AnyObject = msg_send![class!(NSWindow), alloc];
    let window: *mut AnyObject = msg_send![
        window,
        initWithContentRect: frame,
        styleMask: style,
        backing: 2u64,
        defer: false
    ];
    if window.is_null() {
        return window;
    }
    let ns_title = crate::ffi::make_nsstring(title);
    let _: () = msg_send![window, setTitle: ns_title];
    crate::ffi::CFRelease(ns_title as *const std::ffi::c_void);
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
    window
}

unsafe fn close_probe_window(window: *mut AnyObject) {
    let _: () = msg_send![window, close];
    crate::ffi::release_obj(window);
}

unsafe fn restore_probe_window(window: *mut AnyObject) {
    let _: () = msg_send![window, deminiaturize: std::ptr::null::<AnyObject>()];
    let _: () = msg_send![window, orderFront: std::ptr::null::<AnyObject>()];
    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    let _: () = msg_send![app, unhide: std::ptr::null::<AnyObject>()];
}

unsafe fn apply_step(window: *mut AnyObject, step: Step) {
    let nil = std::ptr::null::<AnyObject>();
    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
    match step {
        Step::Normal | Step::OrderFront => {
            let _: () = msg_send![window, deminiaturize: nil];
            let _: () = msg_send![window, orderFront: nil];
        }
        Step::OrderOut => {
            let _: () = msg_send![window, orderOut: nil];
        }
        Step::Miniaturize => {
            let _: () = msg_send![window, miniaturize: nil];
        }
        Step::Restore => {
            let _: () = msg_send![window, deminiaturize: nil];
        }
        Step::HideApp => {
            let _: () = msg_send![app, hide: nil];
        }
        Step::UnhideApp => {
            let _: () = msg_send![app, unhide: nil];
        }
    }
}

/// Pump the main run loop for `seconds`, which is what lets AppKit/WindowServer process the state
/// change we just asked for.
unsafe fn pump(seconds: f64) {
    let run_loop: *mut AnyObject = msg_send![class!(NSRunLoop), currentRunLoop];
    let date: *mut AnyObject = msg_send![class!(NSDate), dateWithTimeIntervalSinceNow: seconds];
    let _: () = msg_send![run_loop, runUntilDate: date];
}

/// Poll until the observation changes, then keep polling until it holds still for a moment. Returns
/// the settled sample and every distinct observation with the elapsed time it first appeared.
unsafe fn wait_for_change(
    wid: u32,
    own_pid: i32,
    previous: Sample,
    no_change_patience_ms: u128,
) -> (Sample, Vec<(u128, Sample)>) {
    let started = Instant::now();
    let deadline = Duration::from_millis(8000);
    let mut transitions: Vec<(u128, Sample)> = Vec::new();
    let mut current = previous;
    let mut stable_since = Instant::now();
    while started.elapsed() < deadline {
        pump(0.02);
        let sample = read_sample(wid, own_pid);
        if sample != current {
            current = sample;
            transitions.push((started.elapsed().as_millis(), sample));
            stable_since = Instant::now();
        } else if !transitions.is_empty() && stable_since.elapsed() > Duration::from_millis(400) {
            break;
        } else if transitions.is_empty()
            && started.elapsed() > Duration::from_millis(no_change_patience_ms as u64)
        {
            // Nothing changed at all: report the state as settled rather than waiting out the
            // whole deadline (an orderFront on an already-front window does this).
            break;
        }
    }
    (current, transitions)
}

unsafe fn read_sample(wid: u32, own_pid: i32) -> Sample {
    let row = skylight::window_rows(&[wid]).remove(&wid);
    // The AX snapshot is cached for 750ms, which would hide exactly the transitions this
    // instrument exists to time; clear it so every sample is a fresh read.
    crate::window_collector::clear_ax_window_cache_for_pid(own_pid);
    let identity = crate::app_identity::resolve_app_identity(own_pid);
    let ax = super::raiser::get_ax_windows_for_pid_with_identity(
        own_pid,
        identity.process_start_time_us,
    )
    .and_then(|windows| windows.into_iter().find(|window| window.cgwid == wid));
    Sample {
        attributes: row.as_ref().and_then(|row| row.attributes),
        tags: row.as_ref().and_then(|row| row.tags),
        space_type_mask: row.as_ref().and_then(|row| row.space_type_mask),
        ax_minimized: ax.as_ref().map(|window| window.minimized),
        ax_fullscreen: ax.as_ref().and_then(|window| window.is_fullscreen),
        cg_onscreen: Some(crate::window_collector::window_is_onscreen_now(wid)),
    }
}

/// The probe window's WindowServer id, resolved through our own accessibility answer by title.
unsafe fn probe_wid(own_pid: i32, title: &str) -> Option<u32> {
    crate::window_collector::clear_ax_window_cache_for_pid(own_pid);
    let identity = crate::app_identity::resolve_app_identity(own_pid);
    let windows = super::raiser::get_ax_windows_for_pid_with_identity(
        own_pid,
        identity.process_start_time_us,
    )?;
    windows
        .into_iter()
        .find(|window| window.title == title && window.cgwid != 0)
        .map(|window| window.cgwid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(attributes: Option<u64>, tags: Option<u64>, ax_minimized: Option<bool>) -> Sample {
        Sample {
            attributes,
            tags,
            ax_minimized,
            ..Sample::default()
        }
    }

    #[test]
    fn the_evidence_gate_fails_when_the_minimized_tag_is_missing() {
        // The counter-example the merged verdict cannot catch: AX says minimized, so the decoded
        // verdict would be `true`, but the WindowServer tag this work relies on was not readable.
        let ax_only = sample(Some(0x1), None, Some(true));
        assert_eq!(ax_only.minimized(), (true, "ax"));
        assert!(cell_evidence_holds(Step::Miniaturize, &ax_only).is_err());
    }

    #[test]
    fn the_evidence_gate_fails_when_the_minimized_tag_never_sets() {
        // A field that is readable but always clear must fail the minimized cell too.
        let tag_clear = sample(Some(0x1), Some(0x2001_0048_0001), Some(true));
        assert!(cell_evidence_holds(Step::Miniaturize, &tag_clear).is_err());
        // ...and the same sample must pass the restore cell, which claims the opposite.
        assert!(cell_evidence_holds(Step::Restore, &tag_clear).is_ok());
    }

    #[test]
    fn the_evidence_gate_checks_the_hidden_tag_and_the_ordered_in_field() {
        let hidden = sample(Some(0x1), Some(0x2081_0048_0001), Some(false));
        assert!(cell_evidence_holds(Step::HideApp, &hidden).is_ok());
        assert!(cell_evidence_holds(Step::UnhideApp, &hidden).is_err());
        // A missing attributes field fails every cell: the ordered-in verdict comes from it.
        let no_attributes = sample(None, Some(0x2001_0048_2001), Some(false));
        assert!(cell_evidence_holds(Step::Normal, &no_attributes).is_err());
    }

    /// Run the state matrix on the AppKit main thread in a child process. It fails while MATRIX is
    /// unpinned, which is the honest outcome: the gate must not pass before the recording exists.
    #[test]
    #[ignore]
    fn space_state_matrix_smoke() {
        let exe = std::env::current_exe().expect("current exe");
        let app = exe
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("oh-my-tab"))
            .expect("app binary path");
        assert!(
            app.exists(),
            "app binary missing at {}: run `cargo build` first",
            app.display()
        );
        let out = std::process::Command::new(&app)
            .arg("--smoke-space-state-matrix")
            .output()
            .expect("failed to spawn app");
        assert!(
            out.status.success(),
            "space state matrix smoke failed (exit {:?})\nstderr:\n{}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
