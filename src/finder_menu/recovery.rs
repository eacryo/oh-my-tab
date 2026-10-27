//! Restoring the Finder Sync extension's system election after an app update.
//!
//! macOS disables a Finder Sync extension when the containing app is replaced by an update, and an
//! extension cannot enable itself. The main app can restore the user's previous choice by changing
//! the system election ('pluginkit -e use'), but only when it can prove the election it restores
//! belonged to the user *before our own update replaced the bundle*: a transaction recorded at the
//! install lifecycle, carrying the expected target build. A manual replacement, a cancelled update,
//! a build we did not schedule, and an unsupported election state never restore anything.
//!
//! The state lives in UserDefaults (cross-launch UI lifecycle, like the onboarding/update-notice
//! markers) and is redirected to files by '--finder-ext-state=<dir>' so tier-A tests never touch
//! production keys. The decision is a pure function; the external commands run on a worker thread
//! with timeouts, never on the main thread.
//!
//! See docs/developer-notes-en.md, "Finder extension election after an app update".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use serde::{Deserialize, Serialize};

use crate::ffi::{make_nsstring, release_obj};

/// The transaction Sparkle's install lifecycle recorded for the update that just replaced the app.
const TRANSACTION_KEY: &str = "oh-my-tab-finder-ext-transaction";
/// One attempt per update transaction: (source_build, target_build, appex_id).
const ATTEMPT_KEY: &str = "oh-my-tab-finder-ext-attempt";
/// The last restore outcome, as '<target_build>\t<outcome>', for the settings hint.
const RESULT_KEY: &str = "oh-my-tab-finder-ext-result";
/// A found update that has not been scheduled for installation (informational only).
const CANDIDATE_KEY: &str = "oh-my-tab-finder-ext-candidate";

const PLUGINKIT: &str = "/usr/bin/pluginkit";
const APPEX_NAME: &str = "Oh-My-Tab-Finder.appex";
const FINDER_SYNC_POINT: &str = "com.apple.FinderSync";

/// A system command may hang on a busy PlugInKit service; the worker is bounded.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
/// How long the election may take to become visible after '-e use'.
const CONFIRM_DEADLINE: Duration = Duration::from_secs(3);
const CONFIRM_INTERVAL: Duration = Duration::from_millis(200);

/// Sparkle's SUInstallationCanceledError (vendor/Sparkle.framework .../SUErrors.h:66).
const SU_INSTALLATION_CANCELED: isize = 4007;
/// Sparkle's error domain (SUErrors.h:27); an installation cancellation only counts inside it.
const SPARKLE_ERROR_DOMAIN: &str = "SUSparkleErrorDomain";

/// False once a smoke process with no isolated store has started: the quit refresh must obey the
/// same boundary kick() does.
static RECOVERY_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Which slot of the state store a key addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Transaction,
    Attempt,
    Result,
    Candidate,
}

impl Slot {
    fn file_name(self) -> &'static str {
        match self {
            Slot::Transaction => "transaction.toml",
            Slot::Attempt => "attempt",
            Slot::Result => "result",
            Slot::Candidate => "candidate.toml",
        }
    }

    fn defaults_key(self) -> &'static str {
        match self {
            Slot::Transaction => TRANSACTION_KEY,
            Slot::Attempt => ATTEMPT_KEY,
            Slot::Result => RESULT_KEY,
            Slot::Candidate => CANDIDATE_KEY,
        }
    }
}

/// The state store: production UserDefaults, or a directory under '--finder-ext-state'.
#[derive(Debug, Clone)]
enum Store {
    Defaults,
    Files(PathBuf),
}

impl Store {
    /// The store this process uses: a files store only when the dev flag names a directory.
    fn current() -> Self {
        match crate::dev_flags::value("finder-ext-state") {
            Some(dir) if !dir.trim().is_empty() => Store::Files(PathBuf::from(dir)),
            _ => Store::Defaults,
        }
    }

    fn load(&self, slot: Slot) -> Option<String> {
        match self {
            Store::Defaults => unsafe { defaults_string(slot.defaults_key()) },
            Store::Files(dir) => std::fs::read_to_string(dir.join(slot.file_name())).ok(),
        }
    }

    fn save(&self, slot: Slot, value: Option<&str>) {
        match self {
            Store::Defaults => unsafe { defaults_set(slot.defaults_key(), value) },
            Store::Files(dir) => {
                let path = dir.join(slot.file_name());
                let outcome = match value {
                    Some(text) => {
                        std::fs::create_dir_all(dir).and_then(|_| std::fs::write(&path, text))
                    }
                    None => match std::fs::remove_file(&path) {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                        Err(error) => Err(error),
                    },
                };
                if let Err(error) = outcome {
                    crate::log_info!("[finder-ext] could not write the recovery state: {error}");
                }
            }
        }
    }
}

/// The update transaction: the app this launch replaced and the build it promised to install.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Transaction {
    source_build: String,
    target_build: String,
    appex_id: String,
    enabled_before: bool,
    #[serde(default)]
    feature_enabled: Option<bool>,
}

impl Transaction {
    fn attempt_key(&self) -> String {
        format!(
            "{}>{}:{}",
            self.source_build, self.target_build, self.appex_id
        )
    }

    /// A transaction is only usable when every field it decides on is present.
    fn is_complete(&self) -> bool {
        !self.source_build.is_empty() && !self.target_build.is_empty() && !self.appex_id.is_empty()
    }
}

/// A found update that has not been scheduled for installation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Candidate {
    target_build: String,
    display_version: String,
}

/// What this launch should do with the stored transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Plan {
    /// Keep the transaction: the update has not been installed yet (or this is its source build).
    Keep,
    /// Consume the transaction without touching the election.
    Discard,
    /// This launch is the first one after the promised update: restore once.
    Restore,
}

/// The system election of one plugin record, from 'pluginkit -m'.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Enabled,
    Ignored,
    Default,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginRecord {
    /// The plugin id without its '(version)' suffix: the election is keyed by the id.
    id: String,
    marker: Marker,
    #[allow(dead_code)]
    uuid: String,
    path: String,
}

/// The outcome of a restore attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Confirmed,
    Unconfirmed,
    IdentityMismatch,
    /// The user turned the Finder menu off while the worker was running.
    FeatureDisabled,
    CommandFailed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Confirmed => "confirmed",
            Outcome::Unconfirmed => "unconfirmed",
            Outcome::IdentityMismatch => "identity-mismatch",
            Outcome::FeatureDisabled => "feature-disabled",
            Outcome::CommandFailed => "command-failed",
        }
    }
}

/// The pure decision: should this launch restore the election?
fn plan(
    transaction: &Transaction,
    current_build: &str,
    feature_enabled: bool,
    enabled_now: bool,
    attempt: Option<&str>,
) -> Plan {
    if !transaction.is_complete() || current_build.is_empty() {
        return Plan::Discard;
    }
    if current_build == transaction.source_build {
        return Plan::Keep;
    }
    if current_build != transaction.target_build {
        return Plan::Discard;
    }
    // The feature switch must be on *now* as well as when the install was scheduled: a user who
    // turns the Finder menu off between the update and the first launch does not want it back.
    let feature = feature_enabled && transaction.feature_enabled.unwrap_or(true);
    if !feature || !transaction.enabled_before || enabled_now {
        return Plan::Discard;
    }
    if attempt == Some(transaction.attempt_key().as_str()) {
        return Plan::Discard;
    }
    Plan::Restore
}

/// 'pluginkit -m' starts the marker column; a missing marker means "not elected" (default).
fn marker_of(line: &str) -> Marker {
    match line.trim_start().chars().next() {
        Some('+') => Marker::Enabled,
        Some('-') => Marker::Ignored,
        // '!' is a deliberate debug election and '='/'?' are not part of the contract; none of them
        // may be read as "default" or as "not enabled", because we would then write over them.
        Some('!') | Some('=') | Some('?') => Marker::Unsupported,
        _ => Marker::Default,
    }
}

/// Parse the tab-separated records 'pluginkit -m' prints: marker, id(version), UUID, date, path.
fn parse_records(stdout: &str) -> Vec<PluginRecord> {
    let mut records = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let marker = marker_of(line);
        let body = line
            .trim_start()
            .trim_start_matches(['+', '-', '!', '=', '?']);
        let mut fields = body.trim_start().split('\t');
        let Some(identity) = fields.next() else {
            continue;
        };
        let identity = identity.trim();
        let uuid = fields.next().unwrap_or("").trim().to_string();
        let _timestamp = fields.next();
        let path = fields.next().unwrap_or("").trim().to_string();
        if path.is_empty() {
            continue;
        }
        // 'id(version)': the version belongs to the record, not to the id the election is keyed by.
        let id = identity
            .split_once('(')
            .map_or(identity, |(id, _)| id)
            .trim()
            .to_string();
        records.push(PluginRecord {
            id,
            marker,
            uuid,
            path,
        });
    }
    records
}

/// The records of our extension id whose path is exactly our appex.
///
/// The identity preflight requires exactly one: a second record at the same path would make "the
/// election we are about to change" ambiguous, and its unsupported state could hide behind it.
fn records_for<'a>(
    records: &'a [PluginRecord],
    appex_id: &str,
    appex: &Path,
) -> Vec<&'a PluginRecord> {
    let wanted = std::fs::canonicalize(appex).unwrap_or_else(|_| appex.to_path_buf());
    records
        .iter()
        .filter(|record| {
            if record.id != appex_id {
                return false;
            }
            let path = Path::new(&record.path);
            let actual = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            actual == wanted
        })
        .collect()
}

/// Full verbose enumeration of the Finder Sync plugins.
///
/// '-A -D' are required: without them PlugInKit reports only the highest version's last instance, so
/// a second copy at our own path could hide behind the folded result. The id filter is applied by us
/// (records_for) rather than '-i', which folds the same way. '-v' prints the tab-separated
/// UUID/timestamp/path record; plain '-m' prints only 'marker id(version)'.
fn list_args() -> Vec<String> {
    vec![
        "-m".into(),
        "-A".into(),
        "-D".into(),
        "-v".into(),
        "-p".into(),
        FINDER_SYNC_POINT.into(),
    ]
}

fn register_args(appex: &Path) -> Vec<String> {
    vec!["-a".into(), appex.to_string_lossy().into_owned()]
}

fn elect_args(appex_id: &str) -> Vec<String> {
    vec!["-e".into(), "use".into(), "-i".into(), appex_id.into()]
}

/// One system command's result.
struct CommandResult {
    stdout: String,
    success: bool,
    #[allow(dead_code)]
    timed_out: bool,
}

/// Run one command with a timeout, draining both pipes so a chatty child cannot deadlock us.
fn run_command(program: &Path, args: &[String], timeout: Duration) -> Option<CommandResult> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Both pipes must be drained concurrently: reading stdout to EOF before touching stderr
    // deadlocks a child that fills the stderr pipe while we are still reading stdout.
    use std::io::Read;
    let drain = |pipe: Option<std::process::ChildStdout>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout_reader = drain(child.stdout.take());
    let stderr_reader = std::thread::spawn({
        let pipe = child.stderr.take();
        move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        }
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // A descendant that inherited the pipe can keep it open after the direct child was killed, so
    // the join is bounded as well: past the grace, the reader is detached rather than waited on.
    let grace_deadline = Instant::now() + Duration::from_millis(200);
    while !(stdout_reader.is_finished() && stderr_reader.is_finished())
        && Instant::now() < grace_deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = if stdout_reader.is_finished() {
        stdout_reader.join().unwrap_or_default()
    } else {
        crate::log_info!("[finder-ext] a pluginkit pipe was still held after the timeout");
        String::new()
    };
    if stderr_reader.is_finished() {
        let _ = stderr_reader.join();
    }
    Some(CommandResult {
        stdout: output,
        success: status.map(|status| status.success()).unwrap_or(false),
        timed_out,
    })
}

/// Restore the election for 'transaction', using 'run' for every system command.
///
/// Split from 'kick' so the whole sequence is testable with an injected runner (no real pluginkit,
/// no real election): the identity preflight refuses to touch the election unless exactly one
/// record at our own appex path exists.
fn list_records<F>(run: &mut F) -> Option<Vec<PluginRecord>>
where
    F: FnMut(&[String]) -> Option<CommandResult>,
{
    run(&list_args()).and_then(|result| {
        if result.success {
            Some(parse_records(&result.stdout))
        } else {
            None
        }
    })
}

fn restore_with<F>(
    transaction: &Transaction,
    appex: &Path,
    appex_id: &str,
    mut recheck: impl FnMut() -> bool,
    mut run: F,
) -> Outcome
where
    F: FnMut(&[String]) -> Option<CommandResult>,
{
    // The restore basis names an appex id: never apply it to a different extension.
    if transaction.appex_id != appex_id {
        return Outcome::IdentityMismatch;
    }
    let mut records = match list_records(&mut run) {
        Some(records) => records,
        None => return Outcome::CommandFailed,
    };
    if records_for(&records, appex_id, appex).is_empty() {
        // The rebuilt bundle is not registered at its own path yet; registering is idempotent and
        // touches no election, so it is safe before deciding anything.
        if run(&register_args(appex)).map(|result| result.success) != Some(true) {
            return Outcome::CommandFailed;
        }
        records = match list_records(&mut run) {
            Some(records) => records,
            None => return Outcome::CommandFailed,
        };
    }
    let at_path = records_for(&records, appex_id, appex);
    let [record] = at_path.as_slice() else {
        return Outcome::IdentityMismatch;
    };
    match record.marker {
        Marker::Enabled => return Outcome::Confirmed,
        // An unsupported election is left exactly as it is: this feature restores the ordinary
        // "use" election only, and '!' is a deliberate debug state.
        Marker::Unsupported => return Outcome::IdentityMismatch,
        Marker::Ignored | Marker::Default => {}
    }
    // The user may have turned the Finder menu off while the worker was enumerating: re-check the
    // feature switch immediately before the one write that changes a system setting.
    if !recheck() {
        return Outcome::FeatureDisabled;
    }
    if run(&elect_args(appex_id)).map(|result| result.success) != Some(true) {
        return Outcome::CommandFailed;
    }
    let deadline = Instant::now() + CONFIRM_DEADLINE;
    loop {
        if let Some(records) = list_records(&mut run) {
            if records_for(&records, appex_id, appex)
                .first()
                .map(|record| record.marker)
                == Some(Marker::Enabled)
            {
                return Outcome::Confirmed;
            }
        }
        if Instant::now() >= deadline {
            return Outcome::Unconfirmed;
        }
        std::thread::sleep(CONFIRM_INTERVAL);
    }
}

/// The appex this build embeds, and its bundle id.
fn embedded_appex() -> Option<(PathBuf, String)> {
    let bundle_path = main_bundle_path()?;
    let appex = bundle_path.join("Contents/PlugIns").join(APPEX_NAME);
    let appex_id = bundle_id_at(&appex)?;
    Some((appex, appex_id))
}

fn main_bundle_path() -> Option<PathBuf> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), mainBundle];
        if bundle.is_null() {
            return None;
        }
        let path: *mut AnyObject = msg_send![bundle, bundlePath];
        if path.is_null() {
            return None;
        }
        let path = crate::ffi::nsstring_to_rust(path);
        (!path.is_empty()).then(|| PathBuf::from(path))
    }
}

fn bundle_id_at(path: &Path) -> Option<String> {
    unsafe {
        let ns_path = make_nsstring(&path.to_string_lossy());
        let bundle: *mut AnyObject = msg_send![class!(NSBundle), bundleWithPath: ns_path];
        release_obj(ns_path);
        if bundle.is_null() {
            return None;
        }
        let identifier: *mut AnyObject = msg_send![bundle, bundleIdentifier];
        if identifier.is_null() {
            return None;
        }
        let identifier = crate::ffi::nsstring_to_rust(identifier);
        (!identifier.is_empty()).then_some(identifier)
    }
}

fn current_build() -> String {
    unsafe { crate::ffi::bundle_info_string("CFBundleVersion") }
}

fn feature_enabled() -> bool {
    crate::config::CONFIG
        .read()
        .map(|config| config.finder_menu.enabled)
        .unwrap_or(false)
}

fn pluginkit_path() -> PathBuf {
    match crate::dev_flags::value("pluginkit-path") {
        Some(path) if !path.trim().is_empty() => PathBuf::from(path),
        _ => PathBuf::from(PLUGINKIT),
    }
}

/// Post the "recovery finished" refresh to the main thread.
fn request_ui_refresh() {
    static TARGET: OnceLock<usize> = OnceLock::new();
    let target = *TARGET.get_or_init(|| {
        let class = refresh_target_class();
        let instance: *mut AnyObject = unsafe { msg_send![class, new] };
        // Retained for the process lifetime on purpose: it is the performSelector target.
        instance as usize
    });
    if target == 0 {
        return;
    }
    unsafe {
        let target = target as *mut AnyObject;
        let _: () = msg_send![
            target,
            performSelectorOnMainThread: sel!(finderExtRecoveryFinished),
            withObject: std::ptr::null_mut::<AnyObject>(),
            waitUntilDone: false
        ];
    }
}

fn refresh_target_class() -> *mut AnyObject {
    static CLASS: OnceLock<crate::ffi::StaticClass> = OnceLock::new();
    let class =
        crate::ffi::StaticClass::register_once(&CLASS, c"OhMyTabFinderExtRecoveryTarget", || {
            crate::ffi::build_class(
                c"OhMyTabFinderExtRecoveryTarget",
                class!(NSObject),
                |builder| unsafe {
                    builder.add_method::<AnyObject, _>(
                        sel!(finderExtRecoveryFinished),
                        on_recovery_finished as unsafe extern "C" fn(*mut AnyObject, Sel),
                    );
                },
            )
        });
    class.0 as *mut AnyObject
}

extern "C" fn on_recovery_finished(_this: *mut AnyObject, _cmd: Sel) {
    crate::callback_guard::void("finder_ext_recovery_finished", || {
        crate::settings::refresh_finder_menu_status_if_visible();
    });
}

/// Decide and, when this launch is the promised update's first launch, start the restore worker.
///
/// Called once at startup on the main thread (the FinderSync and NSBundle reads are main-thread
/// work); the external commands run on the worker.
/// Establish the smoke boundary before the updater starts.
///
/// Smoke children start the real app, and with Sparkle present they can receive update callbacks.
/// Every persistent entry point must see the same decision, so it is taken once, before
/// updater::initialize, and every writer checks writes_allowed().
pub(crate) fn prepare(smoke_process: bool) {
    let isolated = !matches!(Store::current(), Store::Defaults);
    let allowed = !smoke_process || isolated;
    RECOVERY_ENABLED.store(allowed, std::sync::atomic::Ordering::Relaxed);
    if !allowed {
        crate::log_debug!(
            "[finder-ext] recovery is disabled in this smoke process (no isolated store)"
        );
    }
}

/// Whether persistent recovery state may be read or written in this process.
fn writes_allowed() -> bool {
    RECOVERY_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn kick() {
    if !writes_allowed() {
        return;
    }
    let store = Store::current();
    let Some(transaction) = store
        .load(Slot::Transaction)
        .and_then(|text| toml::from_str::<Transaction>(&text).ok())
    else {
        return;
    };
    let build = current_build();
    let attempt = store.load(Slot::Attempt);
    let observed = crate::finder_menu::extension_enabled();
    let decision = plan(
        &transaction,
        &build,
        feature_enabled(),
        observed,
        attempt.as_deref(),
    );
    crate::log_info!(
        "[finder-ext] recovery plan at build {} (source {}, target {}): {:?}",
        build,
        transaction.source_build,
        transaction.target_build,
        decision
    );
    match decision {
        Plan::Keep => {}
        Plan::Discard => store.save(Slot::Transaction, None),
        Plan::Restore => {
            store.save(Slot::Attempt, Some(&transaction.attempt_key()));
            store.save(Slot::Transaction, None);
            let Some((appex, appex_id)) = embedded_appex() else {
                store.save(Slot::Result, Some(&format!("{build}\tno-appex")));
                return;
            };
            let worker_store = store.clone();
            let pluginkit = pluginkit_path();
            let target_build = transaction.target_build.clone();
            std::thread::spawn(move || {
                let outcome =
                    restore_with(&transaction, &appex, &appex_id, feature_enabled, |args| {
                        run_command(&pluginkit, args, COMMAND_TIMEOUT)
                    });
                worker_store.save(
                    Slot::Result,
                    Some(&format!("{target_build}\t{}", outcome.as_str())),
                );
                crate::log_info!(
                    "[finder-ext] restore after update {} -> {}",
                    target_build,
                    outcome.as_str()
                );
                request_ui_refresh();
            });
        }
    }
}

/// A found update: informational only, never a restore basis.
pub(crate) fn note_update_found(target_build: &str, display_version: &str) {
    if target_build.is_empty() || !writes_allowed() {
        return;
    }
    let candidate = Candidate {
        target_build: target_build.to_string(),
        display_version: display_version.to_string(),
    };
    if let Ok(text) = toml::to_string(&candidate) {
        Store::current().save(Slot::Candidate, Some(&text));
    }
}

/// Sparkle's install lifecycle scheduled this update: this is the restore basis.
pub(crate) fn note_install_scheduled(target_build: &str) {
    if target_build.is_empty() || !writes_allowed() {
        return;
    }
    let Some((_, appex_id)) = embedded_appex() else {
        return;
    };
    let transaction = Transaction {
        source_build: current_build(),
        target_build: target_build.to_string(),
        appex_id,
        enabled_before: crate::finder_menu::extension_enabled(),
        feature_enabled: Some(feature_enabled()),
    };
    if let Ok(text) = toml::to_string(&transaction) {
        let store = Store::current();
        store.save(Slot::Candidate, None);
        store.save(Slot::Transaction, Some(&text));
        crate::log_info!(
            "[finder-ext] install scheduled: {} -> {} (extension enabled before: {})",
            transaction.source_build,
            transaction.target_build,
            transaction.enabled_before
        );
    }
}

/// The user pressed Skip. In the install phase that cancels a scheduled installation, which must
/// revoke the restore basis; in the discovery phase it only drops the informational candidate.
pub(crate) fn note_install_skipped(installing: bool, target_build: &str) {
    if !writes_allowed() {
        return;
    }
    skip_in(&Store::current(), installing, target_build);
}

/// Revoke only the transaction this prompt's installation belongs to: a Skip on another update's
/// prompt must not delete a different update that is still scheduled to install at quit.
fn skip_in(store: &Store, installing: bool, target_build: &str) {
    store.save(Slot::Candidate, None);
    if !installing || target_build.is_empty() {
        return;
    }
    let scheduled = store
        .load(Slot::Transaction)
        .and_then(|text| toml::from_str::<Transaction>(&text).ok());
    if scheduled.is_some_and(|transaction| transaction.target_build == target_build) {
        crate::log_info!("[finder-ext] installation skipped; its restore transaction is revoked");
        store.save(Slot::Transaction, None);
    }
}

/// Sparkle reported an update-cycle error. Only an error that terminated the scheduled
/// installation may revoke the transaction; a "no update" result or a later cycle must not.
pub(crate) fn note_update_error(code: isize, domain: &str) {
    if !writes_allowed() {
        return;
    }
    update_error_in(&Store::current(), code, domain);
}

/// Only Sparkle's own installation-cancelled error revokes a transaction. The same numeric code in
/// another domain (or a later cycle's error) says nothing about the scheduled installation.
fn update_error_in(store: &Store, code: isize, domain: &str) {
    if code != SU_INSTALLATION_CANCELED || domain != SPARKLE_ERROR_DOMAIN {
        return;
    }
    if store.load(Slot::Transaction).is_some() {
        crate::log_info!("[finder-ext] installation cancelled; the restore transaction is revoked");
        store.save(Slot::Transaction, None);
    }
}

/// The app is going away: a transaction for an install-on-quit update keeps the state the user
/// leaves behind, so an update installed at quit restores *that* state, not an older one.
pub(crate) fn refresh_before_quit() {
    if !writes_allowed() {
        return;
    }
    let store = Store::current();
    let Some(mut transaction) = store
        .load(Slot::Transaction)
        .and_then(|text| toml::from_str::<Transaction>(&text).ok())
    else {
        return;
    };
    if transaction.source_build != current_build() {
        return;
    }
    transaction.enabled_before = crate::finder_menu::extension_enabled();
    if let Ok(text) = toml::to_string(&transaction) {
        store.save(Slot::Transaction, Some(&text));
    }
}

/// Whether the settings page should say the automatic restore after the last update failed.
pub(crate) fn restore_failed() -> bool {
    restore_failed_in(&Store::current(), &current_build())
}

fn restore_failed_in(store: &Store, build: &str) -> bool {
    let Some(result) = store.load(Slot::Result) else {
        return false;
    };
    let mut parts = result.split('\t');
    let Some(target_build) = parts.next() else {
        return false;
    };
    let outcome = parts.next().unwrap_or("");
    target_build == build && outcome != Outcome::Confirmed.as_str()
}

unsafe fn defaults_string(key: &str) -> Option<String> {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    let value: *mut AnyObject = msg_send![defaults, stringForKey: key_ns];
    release_obj(key_ns);
    if value.is_null() {
        return None;
    }
    let text = crate::ffi::nsstring_to_rust(value);
    (!text.is_empty()).then_some(text)
}

unsafe fn defaults_set(key: &str, value: Option<&str>) {
    let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
    let key_ns = make_nsstring(key);
    match value {
        Some(text) => {
            let value_ns = make_nsstring(text);
            let _: () = msg_send![defaults, setObject: value_ns, forKey: key_ns];
            release_obj(value_ns);
        }
        None => {
            let _: () = msg_send![defaults, removeObjectForKey: key_ns];
        }
    }
    release_obj(key_ns);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transaction() -> Transaction {
        Transaction {
            source_build: "20260101000000".into(),
            target_build: "20260102000000".into(),
            appex_id: "com.example.app.FinderSync".into(),
            enabled_before: true,
            feature_enabled: Some(true),
        }
    }

    fn store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (Store::Files(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn markers_keep_unsupported_elections_out_of_the_decision() {
        assert_eq!(marker_of("+    id(0.2.5)\tU\td\t/p"), Marker::Enabled);
        assert_eq!(marker_of("-    id(0.2.5)\tU\td\t/p"), Marker::Ignored);
        assert_eq!(marker_of("     id(0.2.5)\tU\td\t/p"), Marker::Default);
        for marker in ['!', '=', '?'] {
            assert_eq!(
                marker_of(&format!("{marker}    id(0.2.5)\tU\td\t/p")),
                Marker::Unsupported
            );
        }
    }

    #[test]
    fn records_parse_the_tab_separated_fields() {
        let stdout = "+    com.example.app.FinderSync(0.2.5)\tUUID-1\t2026-10-27 09:15:22 +0000\t/Applications/Oh-My-Tab.app/Contents/PlugIns/F.appex\n";
        let records = parse_records(stdout);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].marker, Marker::Enabled);
        assert_eq!(records[0].uuid, "UUID-1");
        assert_eq!(
            records[0].path,
            "/Applications/Oh-My-Tab.app/Contents/PlugIns/F.appex"
        );
    }

    #[test]
    fn plan_keeps_the_transaction_until_its_target_build_arrives() {
        let tx = transaction();
        assert_eq!(plan(&tx, &tx.source_build, true, false, None), Plan::Keep);
        assert_eq!(
            plan(&tx, &tx.target_build, true, false, None),
            Plan::Restore
        );
        let key = tx.attempt_key();
        assert_eq!(
            plan(&tx, &tx.target_build, true, false, Some(&key)),
            Plan::Discard
        );
        assert_eq!(
            plan(&tx, "20260103000000", true, false, None),
            Plan::Discard
        );
    }

    #[test]
    fn plan_never_fights_the_user_or_the_feature_switch() {
        let mut tx = transaction();
        assert_eq!(
            plan(&tx, &tx.target_build, false, false, None),
            Plan::Discard
        );
        tx.feature_enabled = Some(false);
        assert_eq!(
            plan(&tx, &tx.target_build, true, false, None),
            Plan::Discard
        );
        tx.feature_enabled = Some(true);
        tx.enabled_before = false;
        assert_eq!(
            plan(&tx, &tx.target_build, true, false, None),
            Plan::Discard
        );
        assert_eq!(plan(&tx, &tx.target_build, true, true, None), Plan::Discard);
    }

    #[test]
    fn plan_discards_an_incomplete_transaction() {
        let mut tx = transaction();
        tx.target_build.clear();
        assert_eq!(plan(&tx, &tx.source_build, true, true, None), Plan::Discard);
        tx = transaction();
        tx.appex_id.clear();
        assert_eq!(
            plan(&tx, &tx.target_build, true, false, None),
            Plan::Discard
        );
    }

    #[test]
    fn transaction_matches_the_dev_script_fixture() {
        let fixture = include_str!("../../testdata/finder-ext-transaction.toml");
        let parsed: Transaction = toml::from_str(fixture).expect("fixture parses");
        assert_eq!(parsed.source_build, "20260101000000");
        assert_eq!(parsed.target_build, "20260102000000");
        assert_eq!(parsed.appex_id, "com.example.app.FinderSync");
        assert!(parsed.enabled_before);
        assert_eq!(parsed.feature_enabled, None);
        // The same struct the app writes must round-trip through the same TOML.
        let text = toml::to_string(&parsed).expect("serializes");
        let again: Transaction = toml::from_str(&text).expect("parses");
        assert_eq!(again, parsed);
    }

    fn record(marker: char, path: &str) -> String {
        format!("{marker}    com.example.app.FinderSync(0.2.5)\tUUID-1\t2026-10-27 09:15:22 +0000\t{path}\n")
    }

    fn command(stdout: &str, success: bool) -> CommandResult {
        CommandResult {
            stdout: stdout.to_string(),
            success,
            timed_out: false,
        }
    }

    fn appex_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let appex = dir.path().join(APPEX_NAME);
        std::fs::create_dir_all(&appex).expect("appex dir");
        (dir, appex)
    }

    #[test]
    fn restore_registers_then_elects_and_confirms() {
        let (_dir, appex) = appex_dir();
        let path = appex.to_string_lossy().to_string();
        let mut calls: Vec<String> = Vec::new();
        let mut lists = 0;
        let outcome = restore_with(
            &transaction(),
            &appex,
            "com.example.app.FinderSync",
            || true,
            |args| {
                calls.push(args.join(" "));
                match args[0].as_str() {
                    "-m" => {
                        lists += 1;
                        if lists == 1 {
                            Some(command("", true))
                        } else if lists == 2 {
                            Some(command(&record('-', &path), true))
                        } else {
                            Some(command(&record('+', &path), true))
                        }
                    }
                    "-a" => Some(command("", true)),
                    "-e" => Some(command("", true)),
                    other => panic!("unexpected command {other}"),
                }
            },
        );
        assert_eq!(outcome, Outcome::Confirmed);
        assert!(calls[1].starts_with("-a "), "calls: {calls:?}");
        assert!(
            calls[3].starts_with("-e use -i com.example.app.FinderSync"),
            "calls: {calls:?}"
        );
    }

    #[test]
    fn restore_refuses_a_copy_at_another_path() {
        let (_dir, appex) = appex_dir();
        let mut calls: Vec<String> = Vec::new();
        let outcome = restore_with(
            &transaction(),
            &appex,
            "com.example.app.FinderSync",
            || true,
            |args| {
                calls.push(args.join(" "));
                match args[0].as_str() {
                    "-m" => Some(command(
                        &record('-', "/Applications/Other.app/Contents/PlugIns/F.appex"),
                        true,
                    )),
                    "-a" => Some(command("", true)),
                    _ => panic!("must not elect a copy we could not identify"),
                }
            },
        );
        assert_eq!(outcome, Outcome::IdentityMismatch);
        assert!(calls.iter().all(|call| !call.starts_with("-e")));
    }

    #[test]
    fn restore_leaves_an_unsupported_election_alone() {
        let (_dir, appex) = appex_dir();
        let path = appex.to_string_lossy().to_string();
        let mut calls: Vec<String> = Vec::new();
        let outcome = restore_with(
            &transaction(),
            &appex,
            "com.example.app.FinderSync",
            || true,
            |args| {
                calls.push(args.join(" "));
                match args[0].as_str() {
                    "-m" => Some(command(&record('!', &path), true)),
                    _ => panic!("must not write over a debug election"),
                }
            },
        );
        assert_eq!(outcome, Outcome::IdentityMismatch);
        assert!(calls.iter().all(|call| !call.starts_with("-e")));
    }

    #[test]
    fn restore_reports_unconfirmed_when_the_election_never_shows() {
        let (_dir, appex) = appex_dir();
        let path = appex.to_string_lossy().to_string();
        let outcome = restore_with(
            &transaction(),
            &appex,
            "com.example.app.FinderSync",
            || true,
            |args| match args[0].as_str() {
                "-m" => Some(command(&record('-', &path), true)),
                "-e" => Some(command("", true)),
                other => panic!("unexpected command {other}"),
            },
        );
        assert_eq!(outcome, Outcome::Unconfirmed);
    }

    #[test]
    fn skip_revokes_a_scheduled_installation_but_not_a_found_update() {
        let (store, _dir) = store();
        let text = toml::to_string(&transaction()).expect("serializes");
        store.save(Slot::Transaction, Some(&text));
        store.save(Slot::Candidate, Some("target_build = \"x\"\n"));
        skip_in(&store, false, "");
        assert!(store.load(Slot::Transaction).is_some());
        assert!(store.load(Slot::Candidate).is_none());
        store.save(Slot::Candidate, Some("target_build = \"x\"\n"));
        skip_in(&store, true, &transaction().target_build);
        assert!(store.load(Slot::Transaction).is_none());
    }

    #[test]
    fn only_the_cancelled_installation_error_revokes_the_transaction() {
        let (store, _dir) = store();
        let text = toml::to_string(&transaction()).expect("serializes");
        store.save(Slot::Transaction, Some(&text));
        update_error_in(&store, 1001, SPARKLE_ERROR_DOMAIN);
        assert!(
            store.load(Slot::Transaction).is_some(),
            "a no-update result must not revoke"
        );
        update_error_in(&store, SU_INSTALLATION_CANCELED, SPARKLE_ERROR_DOMAIN);
        assert!(store.load(Slot::Transaction).is_none());
    }

    #[test]
    fn the_hint_follows_the_installed_build() {
        let (store, _dir) = store();
        let build = "20260102000000";
        store.save(Slot::Result, Some(&format!("{build}\tunconfirmed")));
        assert!(restore_failed_in(&store, build));
        assert!(!restore_failed_in(&store, "other"));
        store.save(Slot::Result, Some(&format!("{build}\tconfirmed")));
        assert!(!restore_failed_in(&store, build));
    }

    #[test]
    fn commands_name_the_appex_identifier_and_path() {
        assert_eq!(
            elect_args("com.example.app.FinderSync"),
            vec!["-e", "use", "-i", "com.example.app.FinderSync"]
        );
        // A full verbose enumeration: '-A -D' keep a folded duplicate visible, '-v' carries the
        // path, and the id filter is ours (records_for) because '-i' folds the same way.
        assert_eq!(
            list_args(),
            vec!["-m", "-A", "-D", "-v", "-p", "com.apple.FinderSync"]
        );
        assert_eq!(
            register_args(Path::new("/tmp/x.appex")),
            vec!["-a", "/tmp/x.appex"]
        );
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;

    fn transaction() -> Transaction {
        Transaction {
            source_build: "20260101000000".into(),
            target_build: "20260102000000".into(),
            appex_id: "com.example.app.FinderSync".into(),
            enabled_before: true,
            feature_enabled: Some(true),
        }
    }

    fn appex_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let appex = dir.path().join(APPEX_NAME);
        std::fs::create_dir_all(&appex).expect("appex dir");
        (dir, appex)
    }

    fn record(marker: char, path: &str) -> String {
        format!("{marker}    com.example.app.FinderSync(0.2.5)\tUUID-1\t2026-10-27 09:15:22 +0000\t{path}\n")
    }

    fn command(stdout: &str, success: bool) -> CommandResult {
        CommandResult {
            stdout: stdout.to_string(),
            success,
            timed_out: false,
        }
    }

    #[test]
    fn real_pluginkit_verbose_output_parses() {
        // A recorded 'pluginkit -m -v -p com.apple.FinderSync' transcript: the verbose match is the
        // only form that carries the path, and the parser must read real columns, not a made-up one.
        let stdout = include_str!("../../testdata/pluginkit-verbose.txt");
        let records = parse_records(stdout);
        assert_eq!(records.len(), 2, "records: {records:?}");
        assert_eq!(records[0].marker, Marker::Ignored);
        assert_eq!(records[0].uuid, "E185A411-F347-4F9E-9FFA-ED9568119446");
        assert!(records[0].path.ends_with("baiduNetdiskSync.appex"));
        assert_eq!(records[1].marker, Marker::Enabled);
        assert!(records[1].path.ends_with("Oh-My-Tab-Finder.appex"));
    }

    #[test]
    fn a_second_record_at_our_path_refuses_the_restore() {
        let (_dir, appex) = appex_dir();
        let path = appex.to_string_lossy().to_string();
        let mut calls: Vec<String> = Vec::new();
        let outcome = restore_with(
            &transaction(),
            &appex,
            "com.example.app.FinderSync",
            || true,
            |args| {
                calls.push(args.join(" "));
                match args[0].as_str() {
                    "-m" => Some(command(
                        &format!("{}{}", record('-', &path), record('+', &path)),
                        true,
                    )),
                    _ => panic!("an ambiguous path must not reach an election write"),
                }
            },
        );
        assert_eq!(outcome, Outcome::IdentityMismatch);
        assert!(calls.iter().all(|call| !call.starts_with("-e")));
    }

    #[test]
    fn a_transaction_for_another_appex_is_refused() {
        let (_dir, appex) = appex_dir();
        let mut tx = transaction();
        tx.appex_id = "com.example.other.FinderSync".into();
        let mut calls: Vec<String> = Vec::new();
        let outcome = restore_with(
            &tx,
            &appex,
            "com.example.app.FinderSync",
            || true,
            |args| {
                calls.push(args.join(" "));
                Some(command("", true))
            },
        );
        assert_eq!(outcome, Outcome::IdentityMismatch);
        assert!(
            calls.is_empty(),
            "no command may run for a foreign transaction"
        );
    }

    #[test]
    fn the_feature_switch_is_rechecked_before_writing() {
        let (_dir, appex) = appex_dir();
        let path = appex.to_string_lossy().to_string();
        let mut calls: Vec<String> = Vec::new();
        let outcome = restore_with(
            &transaction(),
            &appex,
            "com.example.app.FinderSync",
            || false,
            |args| {
                calls.push(args.join(" "));
                match args[0].as_str() {
                    "-m" => Some(command(&record('-', &path), true)),
                    _ => panic!("the feature switch is off: no election write"),
                }
            },
        );
        assert_eq!(outcome, Outcome::FeatureDisabled);
        assert!(calls.iter().all(|call| !call.starts_with("-e")));
    }

    #[test]
    fn skipping_another_updates_prompt_keeps_the_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::Files(dir.path().to_path_buf());
        store.save(
            Slot::Transaction,
            Some(&toml::to_string(&transaction()).unwrap()),
        );
        skip_in(&store, true, "19990101000000");
        assert!(store.load(Slot::Transaction).is_some());
        skip_in(&store, true, &transaction().target_build);
        assert!(store.load(Slot::Transaction).is_none());
    }

    #[test]
    fn an_error_outside_sparkles_domain_does_not_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::Files(dir.path().to_path_buf());
        store.save(
            Slot::Transaction,
            Some(&toml::to_string(&transaction()).unwrap()),
        );
        update_error_in(&store, 4007, "NSCocoaErrorDomain");
        assert!(store.load(Slot::Transaction).is_some());
        update_error_in(&store, 4007, SPARKLE_ERROR_DOMAIN);
        assert!(store.load(Slot::Transaction).is_none());
    }

    #[test]
    fn command_runner_survives_stderr_pressure() {
        // A child that fills stderr while we read stdout used to deadlock the sequential reader.
        let script = "i=0; while [ $i -lt 4000 ]; do echo x 1>&2; i=$((i+1)); done; echo done";
        let result = run_command(
            Path::new("/bin/sh"),
            &["-c".to_string(), script.to_string()],
            Duration::from_secs(10),
        )
        .expect("command runs");
        assert!(result.success, "stderr pressure must not fail the command");
        assert!(
            result.stdout.contains("done"),
            "stdout: {:?}",
            result.stdout
        );
        assert!(!result.timed_out);
    }

    #[test]
    fn command_runner_bounds_a_hanging_child() {
        let started = Instant::now();
        let result = run_command(
            Path::new("/bin/sh"),
            &["-c".to_string(), "sleep 5".to_string()],
            Duration::from_millis(200),
        )
        .expect("command starts");
        assert!(!result.success);
        assert!(result.timed_out);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the timeout must bound the call, elapsed {:?}",
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod isolation_tests {
    use super::*;

    /// The persistent writers must obey the smoke boundary: a smoke process with no isolated store
    /// may not touch the production recovery keys, even when Sparkle delivers update callbacks.
    #[test]
    fn writers_do_nothing_when_recovery_is_disabled() {
        // The flag is process-global, and no other test calls the persistent writers, so this
        // cannot race with them.
        RECOVERY_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
        note_update_found("20260102000000", "9.9");
        note_install_scheduled("20260102000000");
        note_install_skipped(true, "20260102000000");
        note_update_error(SU_INSTALLATION_CANCELED, SPARKLE_ERROR_DOMAIN);
        let store = Store::Defaults;
        assert!(store.load(Slot::Transaction).is_none());
        assert!(store.load(Slot::Candidate).is_none());
        assert!(store.load(Slot::Attempt).is_none());
        RECOVERY_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
