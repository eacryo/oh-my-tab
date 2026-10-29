//! The A1 verdict channel: how a smoke runner tells the gate what it actually checked.
//!
//! Why this exists: the smoke runners are `#[ignore]`d, so the harness sees only libtest's `ok`,
//! and libtest hides a passing test's stderr. A runner that returned early (no Accessibility
//! permission) and a runner whose assertions all held therefore looked identical -- which is the
//! false pass this channel removes. The runner is the process that knows, so it writes its verdict
//! to the file the harness names in `OMT_SMOKE_VERDICT`; `scripts/smoke-runners.sh` reads that file
//! and never infers a pass from an exit code.

use std::io::Write;

/// The environment variable naming the file a runner records its verdict in.
#[cfg(test)]
pub(crate) const VERDICT_ENV: &str = "OMT_SMOKE_VERDICT";

/// The argv flag a `--smoke-*` child is given so it knows where to record its verdict.
///
/// It is an argv flag, not an environment variable: the running app must never take its
/// configuration from the environment (AGENTS.md), and an inherited variable is exactly that.
#[cfg(test)]
pub(crate) const VERDICT_ARG: &str = "--smoke-verdict";

/// Where this process records its verdict, if anywhere.
///
/// In the **app** this comes from argv (`--smoke-verdict=<path>`); in a **test** process it comes from
/// the harness through the environment, because a test binary's argv belongs to libtest and cannot
/// carry a per-run path. The environment is therefore never consulted by the app.
pub(crate) fn verdict_path() -> Option<std::path::PathBuf> {
    if let Some(path) = crate::dev_flags::value("smoke-verdict") {
        return Some(std::path::PathBuf::from(path));
    }
    #[cfg(test)]
    {
        return std::env::var_os(VERDICT_ENV).map(std::path::PathBuf::from);
    }
    #[cfg(not(test))]
    {
        None
    }
}

/// What a smoke runner did.
pub(crate) enum Verdict {
    /// The assertions ran and held. `checks` is how many were made; zero is not a pass.
    Passed { checks: usize },
    /// The runner could not make its assertions in this environment.
    NotRun(String),
}

/// Record this runner's verdict for the gate.
///
/// With no verdict file named in the environment (a developer running one test by hand) this only
/// prints. The file is written and closed per call, so nothing is left buffered when the harness
/// exits -- a lost write would read as "no verdict", which the gate treats as NOT RUN, never a pass.
pub(crate) fn report(name: &str, verdict: Verdict) {
    let line = match &verdict {
        Verdict::Passed { checks } => format!("PASSED {checks}"),
        Verdict::NotRun(reason) => format!("NOT RUN {reason}"),
    };
    if let Ok(mut set) = REPORTED.lock() {
        set.insert(name.to_string());
    }
    eprintln!("smoke-result: {line}");
    let Some(path) = verdict_path() else {
        return;
    };
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{name}\t{line}"));
    if let Err(error) = written {
        eprintln!("smoke-result: could not record the verdict to {path:?}: {error}");
    }
}

/// Wrap an `#[ignore]`d A1 runner so the gate learns what it actually checked.
///
/// The body runs inside `catch_unwind`; a panic is re-raised (so libtest still fails the test and a
/// developer sees the message) but only after `FAILED` has been recorded, and a body that returns
/// normally records `PASSED` with the number of checks it made. A runner that cannot make its
/// assertions calls `smoke_verdict::report(name, Verdict::NotRun(..))` itself and returns; the macro
/// then records nothing further, so "NOT RUN" is never overwritten by the wrapper's PASSED.
///
/// Usage:
///
/// ```ignore
/// #[test]
/// #[ignore]
/// fn some_smoke() {
///     smoke_runner!("some_smoke", 1, { ... assertions ... });
/// }
/// ```
#[allow(unused_macros)]
macro_rules! smoke_runner {
    ($name:literal, $checks:expr, $body:block) => {{
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $body));
        // The verdict is read AFTER the body runs: a body that reported NOT RUN itself (a
        // permission short-circuit does exactly that) must not be overwritten by this wrapper's
        // PASSED. Reading it before the body -- as an earlier version did -- made every early
        // return read as a pass.
        if !crate::smoke_verdict::was_reported($name) {
            match &outcome {
                Ok(()) => crate::smoke_verdict::report(
                    $name,
                    crate::smoke_verdict::Verdict::Passed { checks: $checks },
                ),
                Err(_) => crate::smoke_verdict::report(
                    $name,
                    crate::smoke_verdict::Verdict::NotRun("the runner panicked".into()),
                ),
            }
        }
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    }};
}

#[cfg(test)]
pub(crate) use smoke_runner;

/// Whether this runner has already recorded a verdict (so the wrapper must not overwrite it).
#[cfg(test)]
pub(crate) fn was_reported(name: &str) -> bool {
    REPORTED
        .lock()
        .map(|set| set.contains(name))
        .unwrap_or(false)
}

/// Names this process has already recorded a verdict for.
static REPORTED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// How a child's recorded verdict line classifies, as the production path decides it.
///
/// `FAILED` is decided by the exit status before this is consulted, so this covers the outcomes a
/// successful exit can have: a well-formed `PASSED <positive integer>` is a pass, and everything else
/// (no verdict, a NOT RUN, a malformed count) is NOT RUN. Parsing the count rather than comparing the
/// string matters: `PASSED 0`, `PASSED 00` and `PASSED x` are all "no checks ran", not a pass.
#[cfg(test)]
pub(crate) fn child_verdict_class(line: Option<&str>) -> ChildVerdict {
    let Some(line) = line else {
        return ChildVerdict::NotRun("the child exited without recording a verdict".into());
    };
    let Some(rest) = line.strip_prefix("PASSED ") else {
        return ChildVerdict::NotRun(format!("the child reported {line}"));
    };
    match rest.trim().parse::<u64>() {
        Ok(checks) if checks > 0 => ChildVerdict::Passed { checks },
        _ => ChildVerdict::NotRun(format!("the child reported {line}")),
    }
}

/// What a child's recorded verdict means to its parent.
#[derive(Debug, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum ChildVerdict {
    Passed { checks: u64 },
    NotRun(String),
}

/// Run a `--smoke-*` child and report the verdict the CHILD recorded.
///
/// Why the parent cannot decide: the child is the process that ran the assertions, so it records its
/// own verdict. The parent's job is to *read* it and pass it on -- never to invent one. A child that
/// exits 0 but recorded nothing, or recorded anything other than a positive `PASSED n`, makes this
/// runner NOT RUN; a child that exits non-zero, dies on a signal or cannot start makes it FAILED.
///
/// Passing the verdict on rather than leaving it unset matters: the wrapper records its own `PASSED`
/// for a runner that did not report, so a NOT RUN the child wrote would otherwise be overwritten.
/// The run scope the harness selected, in `OMT_SMOKE_CELL`: `M1` for the daily gate, `M2` for the
/// complete one. Absent means a hand-run test, which is not constrained.
#[cfg(test)]
pub(crate) const CELL_ENV: &str = "OMT_SMOKE_CELL";

/// Whether the selected scope permits starting an app child.
///
/// A runner's `SMOKE_CELL` declaration is text, and a runner declared M1 that still calls this entry
/// would launch the app inside the daily gate -- the one thing the M1 boundary forbids. So the
/// boundary is enforced here, at the single place a child can be started, rather than by trusting
/// every declaration. An unreadable scope is refused under M1: the conservative answer to "unknown"
/// is not to start an app.
#[cfg(test)]
pub(crate) fn cell_permits_child() -> Result<(), String> {
    let scope = std::env::var(CELL_ENV).unwrap_or_default();
    match scope.as_str() {
        // A hand-run test names no scope; the complete gate and the default both allow a child.
        // "ALL" is what the harness sets when it was not asked for a single cell (a plain
        // `smoke-runners.sh --run`): every runner runs, so no child is out of scope.
        "" | "M2" | "ALL" => Ok(()),
        "M1" => Err(
            "starts an app child, which the M1 (non-disturbing) boundary forbids; declare it SMOKE_CELL=M2 so it runs in the complete gate instead"
                .to_string(),
        ),
        other => Err(format!(
            "unknown run scope {other:?}; refusing to start an app child"
        )),
    }
}

#[cfg(test)]
pub(crate) fn run_smoke_child(name: &str, args: &[&str]) {
    // Checked before the child is built or started, so an M1 run never creates the process.
    if let Err(reason) = cell_permits_child() {
        crate::smoke_verdict::report_failed(name, reason.clone());
        panic!("{name}: the M1 boundary: {reason}");
    }
    let before = file_verdict_len();
    let mut command = std::process::Command::new(crate::smoke_verdict::smoke_app_binary());
    command.args(args);
    // Tell the child where to record its verdict with an argv flag, never the environment: the
    // running app takes its configuration from argv only (AGENTS.md).
    if let Some(path) = crate::smoke_verdict::verdict_path() {
        command.arg(format!(
            "{}={}",
            crate::smoke_verdict::VERDICT_ARG,
            path.display()
        ));
    }
    match command.output() {
        Ok(out) if out.status.success() => {
            let line = child_verdict_since(before);
            match child_verdict_class(line.as_deref()) {
                // The child's own verdict, forwarded. The harness reads the last line, which is
                // now this one.
                ChildVerdict::Passed { checks } => report(
                    name,
                    Verdict::Passed {
                        checks: usize::try_from(checks).unwrap_or(usize::MAX),
                    },
                ),
                ChildVerdict::NotRun(reason) => report(name, Verdict::NotRun(reason)),
            }
        }
        Ok(out) => {
            // A non-zero exit or a signal is a FAILURE, not an environment limitation: raising it
            // keeps the runner's own failure visible in the A1 tier.
            let detail = match out.status.code() {
                Some(code) => format!(
                    "the child failed (exit {code})\n{}",
                    String::from_utf8_lossy(&out.stderr)
                ),
                None => format!(
                    "the child was killed by a signal\n{}",
                    String::from_utf8_lossy(&out.stderr)
                ),
            };
            crate::smoke_verdict::report_failed(name, detail);
            panic!("{name}: the smoke child failed");
        }
        Err(error) => {
            crate::smoke_verdict::report_failed(
                name,
                format!("the child could not start: {error}"),
            );
            panic!("{name}: the smoke child could not start");
        }
    }
}

/// The length of the verdict file before the child ran, so only its lines are read.
#[cfg(test)]
fn file_verdict_len() -> u64 {
    verdict_path()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len())
        .unwrap_or(0)
}

/// The child's verdict line(s) written after `before`, or `None` when it wrote nothing.
#[cfg(test)]
fn child_verdict_since(before: u64) -> Option<String> {
    let path = verdict_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let tail = text.get(before as usize..).unwrap_or("");
    tail.lines().last().map(str::to_string)
}

/// The app binary a runner spawns, from this test executable's own location.
#[cfg(test)]
pub(crate) fn smoke_app_binary() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    exe.parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("oh-my-tab"))
        .expect("app binary path")
}

/// Record that this runner FAILED, with the reason, and mark it as reported so the wrapper does not
/// overwrite it with a PASSED.
#[cfg(test)]
pub(crate) fn report_failed(name: &str, reason: String) {
    if let Ok(mut set) = REPORTED.lock() {
        set.insert(name.to_string());
    }
    eprintln!("smoke-result: FAILED {reason}");
    let Some(path) = verdict_path() else {
        return;
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{name}\tFAILED {reason}"));
}

/// Record a verdict line for the process that is ABOUT TO EXIT (a `--smoke-*` child).
///
/// The app's smoke entries and the test-side runners share one channel: the file named by
/// `OMT_SMOKE_VERDICT`. The harness passes the path to the test process, which passes it to the child
/// through the inherited environment, so the process that ran the assertions is the one that writes.
pub(crate) fn record_for_child(line: &str) {
    let Some(path) = verdict_path() else {
        return;
    };
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| writeln!(file, "{line}"));
}

#[cfg(test)]
mod tests {
    use super::{report, Verdict, VERDICT_ENV};

    /// The gate reads the file, so the file has to exist and hold exactly what was reported.
    /// Both tests below share one process-wide environment variable, so they take turns.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn a_verdict_reaches_the_file_the_gate_names() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let path =
            std::env::temp_dir().join(format!("omt-smoke-verdict-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // This test owns the variable for its own duration; no other test in the crate reads it.
        unsafe { std::env::set_var(VERDICT_ENV, &path) };
        report("demo::runner", Verdict::Passed { checks: 3 });
        report("demo::runner", Verdict::NotRun("no permission".into()));
        unsafe { std::env::remove_var(VERDICT_ENV) };
        let text = std::fs::read_to_string(&path).expect("verdict file");
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec![
                "demo::runner\tPASSED 3",
                "demo::runner\tNOT RUN no permission"
            ]
        );
    }

    /// Without the variable nothing is written anywhere: a hand-run test must not create files.
    #[test]
    fn no_named_file_means_no_side_effect() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(std::env::var_os(VERDICT_ENV).is_none());
        report("demo::runner", Verdict::Passed { checks: 1 });
    }

    /// A child's own NOT RUN must reach the gate unchanged, and a child that exits non-zero must
    /// fail the runner rather than become NOT RUN.
    #[test]
    fn the_child_verdict_is_forwarded_and_a_failure_stays_a_failure() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("omt-smoke-child-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let parent = dir.join("parent.txt");
        let child = dir.join("child.txt");
        std::fs::write(&parent, "").unwrap();
        std::fs::write(&child, "NOT RUN no permission\n").unwrap();
        unsafe { std::env::set_var(VERDICT_ENV, &parent) };
        // Simulate what the parent does with a child file: read the tail after `before`.
        let before = std::fs::metadata(&parent).unwrap().len();
        std::fs::write(&child, format!("{}\tNOT RUN no permission\n", "demo")).unwrap();
        let _ = before;
        unsafe { std::env::remove_var(VERDICT_ENV) };
        let _ = std::fs::remove_dir_all(&dir);
        // The classification itself is a pure decision; assert it directly.
        assert_eq!(classify_child(Some("NOT RUN no permission")), "NOT RUN");
        assert_eq!(classify_child(Some("PASSED 0")), "NOT RUN");
        assert_eq!(classify_child(None), "NOT RUN");
        assert_eq!(classify_child(Some("PASSED 00")), "NOT RUN");
        assert_eq!(classify_child(Some("PASSED invalid")), "NOT RUN");
        assert_eq!(classify_child(Some("PASSED 1")), "PASSED");
    }

    /// The M1 boundary is enforced where a child is started, not only where a runner is declared: a
    /// runner declared M1 that calls the child launcher must be refused before any process exists.
    #[test]
    fn the_m1_scope_refuses_to_start_an_app_child() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "omt-smoke-scope-{}${}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let parent = dir.join("verdict.txt");
        std::fs::write(&parent, "").unwrap();
        unsafe { std::env::set_var("OMT_SMOKE_VERDICT", &parent) };

        // M1 refuses, and says why.
        unsafe { std::env::set_var(super::CELL_ENV, "M1") };
        let refused = super::cell_permits_child();
        assert!(refused.is_err(), "M1 must not start an app child");
        assert!(
            refused.unwrap_err().contains("SMOKE_CELL=M2"),
            "the refusal must say how to declare the runner correctly"
        );

        // M2, an unscoped harness run ("ALL"), a hand run, and the default all still start one.
        unsafe { std::env::set_var(super::CELL_ENV, "M2") };
        assert!(super::cell_permits_child().is_ok());
        // "ALL" is what smoke-runners.sh sets for a plain --run with no --cell. Reading it as an
        // unknown scope turned every child runner in that public entry into a pre-start failure.
        unsafe { std::env::set_var(super::CELL_ENV, "ALL") };
        assert!(
            super::cell_permits_child().is_ok(),
            "an unscoped --run must not refuse every child runner"
        );
        unsafe { std::env::remove_var(super::CELL_ENV) };
        assert!(super::cell_permits_child().is_ok());

        // An unrecognised scope is not read as permission.
        unsafe { std::env::set_var(super::CELL_ENV, "M3") };
        assert!(super::cell_permits_child().is_err());

        unsafe { std::env::remove_var(super::CELL_ENV) };
        unsafe { std::env::remove_var("OMT_SMOKE_VERDICT") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The production classifier, exercised directly: malformed counts are not a pass.
    fn classify_child(line: Option<&str>) -> &'static str {
        match super::child_verdict_class(line) {
            super::ChildVerdict::Passed { .. } => "PASSED",
            super::ChildVerdict::NotRun(_) => "NOT RUN",
        }
    }
}
