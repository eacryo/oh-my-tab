use flume::{Receiver, Sender};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use crate::ffi::{localtime_r, Tm};

/// Only two levels: Debug (diagnostic detail) / Info (normal runtime info).
/// Errors/warnings all go through log_info! (content preserved, no separate tiers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
        }
    }
}

pub struct LogConfig {
    pub level: LogLevel,
    pub file_path: String, // empty = default rolling path
}

static LOG_TX: OnceLock<Sender<String>> = OnceLock::new();
static LOG_LEVEL: AtomicUsize = AtomicUsize::new(LogLevel::Info as usize);
/// The active log file path resolved at init, for features like "export logs".
static ACTIVE_LOG_PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();

/// The active log file's absolute path (None = no file output this session; in practice
/// only observable before init).
pub fn active_log_path() -> Option<std::path::PathBuf> {
    ACTIVE_LOG_PATH.get().cloned().flatten()
}

// Bounded channel capacity: ~100KB worst case, far larger than a single summon's burst;
// logs are dropped only when disk writes stall severely.
const LOG_CHANNEL_CAPACITY: usize = 512;
const LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;
const LOG_BACKUP_COUNT: usize = 5;

/// Diagnostic detail (per-event / enumeration / sorting), emitted only at the Debug tier.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Debug, format_args!($($arg)*))
    };
}
/// Normal runtime info (startup / toggles / menu / error notices), emitted at both tiers.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Info, format_args!($($arg)*))
    };
}

/// Core function called by macros: threshold filter + timestamp + send to channel.
pub fn _log(level: LogLevel, args: fmt::Arguments<'_>) {
    let threshold =
        unsafe { std::mem::transmute::<usize, LogLevel>(LOG_LEVEL.load(Ordering::Relaxed)) };
    if level < threshold {
        return;
    }
    if let Some(tx) = LOG_TX.get() {
        let ts = now_timestamp();
        let msg = format!("{} {} {}\n", ts, level.as_str(), args);
        // Bounded channel: drop the newest entry when full; never block the caller
        // (logging must not stall the UI / event loop).
        let _ = tx.try_send(msg);
    }
}

/// Call early on the main thread to start the background writer thread.
pub fn init(config: &LogConfig, is_dev: bool) {
    let (tx, rx) = flume::bounded::<String>(LOG_CHANNEL_CAPACITY);
    LOG_TX.set(tx).ok();
    LOG_LEVEL.store(config.level as usize, Ordering::Relaxed);

    let file_path = resolve_file_path(config);
    ACTIVE_LOG_PATH
        .set(file_path.as_ref().map(|dest| dest.path.clone()))
        .ok();
    std::thread::Builder::new()
        .name("log-writer".into())
        .spawn(move || {
            crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Background);
            writer_loop(rx, is_dev, file_path);
        })
        .expect("spawn log-writer thread");
    // Redirect stderr into the log pipeline: system output like NSLog/AppKit warnings
    // (e.g. Menu_Tracking internals) and panics go through stderr; without capture they
    // are invisible in our log (only the terminal / unified log sees them).
    capture_stderr();
}

/// Runtime log level adjustment (triggered by reload_config).
pub fn reconfigure(level: LogLevel) {
    LOG_LEVEL.store(level as usize, Ordering::Relaxed);
}

fn writer_loop(rx: Receiver<String>, is_dev: bool, file_path: Option<LogDestination>) {
    let mut file = file_path.and_then(RollingLogFile::open);

    // Write a session boundary marker on every launch so a stable log file still separates runs.
    let startup = format!("{} INFO  [logger] session started\n", now_timestamp());
    if is_dev {
        print!("{}", startup);
    }
    if let Some(ref mut f) = file {
        f.write_line(&startup);
    }

    while let Ok(msg) = rx.recv() {
        if is_dev {
            print!("{}", msg); // msg already contains \n
        }
        if let Some(ref mut f) = file {
            f.write_line(&msg);
        }
    }
}

struct LogDestination {
    path: PathBuf,
    rolling: bool,
}

struct RollingLogFile {
    writer: BufWriter<std::fs::File>,
    path: PathBuf,
    rolling: bool,
    bytes_written: u64,
}

impl RollingLogFile {
    fn open(destination: LogDestination) -> Option<Self> {
        let bytes_written = match fs::metadata(&destination.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                eprintln!(
                    "[logger] cannot inspect log file {}: {}",
                    destination.path.display(),
                    error
                );
                0
            }
        };
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&destination.path)
            .map_err(|e| {
                // fallback to stderr on writer thread failure
                eprintln!(
                    "[logger] cannot open log file {}: {}",
                    destination.path.display(),
                    e
                );
            })
            .ok()?;
        Some(Self {
            writer: BufWriter::new(file),
            path: destination.path,
            rolling: destination.rolling,
            bytes_written,
        })
    }

    fn write_line(&mut self, msg: &str) {
        let msg_len = msg.len() as u64;
        if self.rolling
            && self.bytes_written > 0
            && self.bytes_written.saturating_add(msg_len) > LOG_MAX_BYTES
        {
            if let Err(error) = self.rotate() {
                eprintln!(
                    "[logger] cannot rotate log file {}: {}",
                    self.path.display(),
                    error
                );
            }
        }
        if self.writer.write_all(msg.as_bytes()).is_ok() {
            self.bytes_written = self.bytes_written.saturating_add(msg_len);
        }
        // flush each line: at most 1 msg lost on crash
        let _ = self.writer.flush();
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;
        let oldest = backup_path(&self.path, LOG_BACKUP_COUNT);
        if oldest.exists() {
            fs::remove_file(oldest)?;
        }
        for index in (1..LOG_BACKUP_COUNT).rev() {
            let source = backup_path(&self.path, index);
            if source.exists() {
                fs::rename(source, backup_path(&self.path, index + 1))?;
            }
        }
        fs::rename(&self.path, backup_path(&self.path, 1))?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.writer = BufWriter::new(file);
        self.bytes_written = 0;
        Ok(())
    }
}

fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(format!(".{index}"));
    PathBuf::from(value)
}

extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut std::ffi::c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
}

/// Redirect the process's stderr (fd 2) to a pipe; a reader thread hands each line to
/// the log pipeline at Info level. NSLog's visible output, AppKit internal warnings and
/// Rust panics all write to stderr; after redirection they appear in the normal log
/// format (timestamp + INFO) with a `[stderr]` prefix, instead of only in the terminal.
/// In dev mode the writer loop still prints to stdout, so the terminal keeps showing them.
fn capture_stderr() {
    unsafe {
        let mut fds: [c_int; 2] = [0; 2];
        if pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        let read_fd = fds[0];
        let write_fd = fds[1];
        if dup2(write_fd, 2) < 0 {
            let _ = close(read_fd);
            let _ = close(write_fd);
            return;
        }
        let _ = close(write_fd);
        std::thread::Builder::new()
            .name("stderr-reader".into())
            .spawn(move || {
                crate::performance::set_current_thread_qos(
                    crate::performance::ThreadQos::Background,
                );
                stderr_reader(read_fd);
            })
            .expect("spawn stderr-reader thread");
    }
}

/// Reader thread: blocks on the pipe, splits lines on \n (buffering partial lines),
/// strips trailing \r, and emits each line at Info level. If the pipe fills, this
/// thread blocks in read while the writer thread keeps draining; memory stays bounded.
fn stderr_reader(fd: c_int) {
    let mut buf = vec![0u8; 4096];
    let mut pending: Vec<u8> = Vec::new();
    unsafe {
        loop {
            let n = read(fd, buf.as_mut_ptr() as *mut std::ffi::c_void, buf.len());
            if n <= 0 {
                break;
            }
            let mut start = 0usize;
            for i in 0..n as usize {
                if buf[i] == b'\n' {
                    pending.extend_from_slice(&buf[start..i]);
                    let line = std::mem::take(&mut pending);
                    emit_stderr_line(&line);
                    start = i + 1;
                }
            }
            pending.extend_from_slice(&buf[start..n as usize]);
        }
    }
    // EOF (unreachable before process exit in practice): flush any trailing partial line.
    if !pending.is_empty() {
        emit_stderr_line(&pending);
    }
    unsafe {
        let _ = close(fd);
    }
}

fn emit_stderr_line(line: &[u8]) {
    // NSLog lines can end with \r (legacy line ending); strip only the \r.
    let trimmed = if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    };
    let s = String::from_utf8_lossy(trimmed).into_owned();
    _log(LogLevel::Info, format_args!("[stderr] {}", s));
}

fn resolve_file_path(config: &LogConfig) -> Option<LogDestination> {
    // Dev mode also writes to a file: writer_loop prints to stdout AND the file when is_dev,
    // so dev logs persist (not lost when the terminal closes). The 30-day cleanup applies
    // to dev logs too.
    // User-supplied path: use verbatim (append mode, no rotation, no cleanup - the user
    // manages rotation themselves). We never write extra files into, or delete files from,
    // a user-specified location.
    if !config.file_path.is_empty() {
        return Some(LogDestination {
            path: PathBuf::from(&config.file_path),
            rolling: false,
        });
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!("{}/Library/Logs/oh-my-tab", home);
    let _ = std::fs::create_dir_all(&dir);
    // Prune logs older than 30 days at startup (default dir and app-owned logs only); never delete the active file.
    cleanup_old_logs(Path::new(&dir));
    Some(LogDestination {
        path: Path::new(&dir).join("oh-my-tab.log"),
        rolling: true,
    })
}

/// Delete app-owned log files and rolling backups in the log dir whose mtime is older than 30 days.
/// Judged by mtime only; the current file's mtime keeps updating as we write, so it's never pruned.
/// Only the default log dir is touched - never the directory of a user-supplied path.
fn cleanup_old_logs(dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let cutoff = match SystemTime::now().checked_sub(Duration::from_secs(30 * 86_400)) {
        Some(c) => c,
        None => return,
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Keep the active rolling file; prune backups and legacy per-launch logs by age.
        let is_backup = name
            .strip_prefix("oh-my-tab.log.")
            .is_some_and(|suffix| suffix.parse::<usize>().is_ok());
        let is_legacy = name.starts_with("oh-my-tab-") && name.ends_with(".log");
        if name == "oh-my-tab.log" || (!is_backup && !is_legacy) {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                if mtime < cutoff {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

// Tm and localtime_r live in ffi.rs now

/// Zero-dep ISO-8601 timestamp with milliseconds.
fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            ms,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_to_padded_labels() {
        assert_eq!(LogLevel::Debug.as_str(), "DEBUG");
        assert_eq!(LogLevel::Info.as_str(), "INFO ");
        assert!(LogLevel::Debug < LogLevel::Info);
    }

    #[test]
    fn timestamps_are_well_formed() {
        // Timestamp format: ISO-ish with T and milliseconds.
        let ts = now_timestamp();
        assert_eq!(ts.len(), 23); // "YYYY-MM-DDTHH:MM:SS.mmm"
        assert!(ts.contains('T'));
    }

    #[test]
    fn cleanup_old_logs_only_touches_stale_own_logs() {
        let dir = tempfile::tempdir().unwrap();
        let old = SystemTime::now()
            .checked_sub(Duration::from_secs(31 * 86_400))
            .unwrap();
        let fresh = SystemTime::now();
        let make = |name: &str, mtime: SystemTime| {
            let p = dir.path().join(name);
            std::fs::write(&p, "x").unwrap();
            let f = std::fs::File::open(&p).unwrap();
            f.set_modified(mtime).unwrap();
            p
        };
        // stale own log: removed.
        let stale = make("oh-my-tab-2020-01-01_00-00-00.log", old);
        // fresh own log: kept.
        let fresh_log = make("oh-my-tab-2099-01-01_00-00-00.log", fresh);
        // stale but unrelated: skipped during cleanup.
        let foreign = make("something-else.log", old);
        cleanup_old_logs(dir.path());
        assert!(!stale.exists());
        assert!(fresh_log.exists());
        assert!(foreign.exists());
    }

    #[test]
    fn rolling_log_rotates_and_keeps_numbered_backups() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oh-my-tab.log");
        std::fs::write(&path, "active").unwrap();
        std::fs::write(backup_path(&path, 1), "older").unwrap();

        let destination = LogDestination {
            path: path.clone(),
            rolling: true,
        };
        let mut log = RollingLogFile::open(destination).unwrap();
        log.rotate().unwrap();

        assert_eq!(
            std::fs::read_to_string(backup_path(&path, 1)).unwrap(),
            "active"
        );
        assert_eq!(
            std::fs::read_to_string(backup_path(&path, 2)).unwrap(),
            "older"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    #[test]
    fn rolling_log_rotates_before_writing_past_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("oh-my-tab.log");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(LOG_MAX_BYTES).unwrap();

        let destination = LogDestination {
            path: path.clone(),
            rolling: true,
        };
        let mut log = RollingLogFile::open(destination).unwrap();
        log.write_line("next message\n");

        assert_eq!(
            std::fs::metadata(backup_path(&path, 1)).unwrap().len(),
            LOG_MAX_BYTES
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "next message\n");
    }
}
