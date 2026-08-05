use flume::{Receiver, Sender};
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::os::raw::c_int;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

// ========== 日志级别 / log level ==========

/// 只有两档:Debug(调试细节)/ Info(常规运行信息)。
/// 错误/警告统一走 log_info!(内容保留,不再单独分级)。
///
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

// ========== 配置 / config ==========

pub struct LogConfig {
    pub level: LogLevel,
    pub file_path: String, // 空=默认路径 / empty = default path
}

// ========== 全局状态 / global state ==========

static LOG_TX: OnceLock<Sender<String>> = OnceLock::new();
static LOG_LEVEL: AtomicUsize = AtomicUsize::new(LogLevel::Info as usize);

// 有界通道容量:最坏约 100KB,远大于单次召唤的日志突发;仅在落盘严重卡顿时才会丢日志。
// Bounded channel capacity: ~100KB worst case, far larger than a single summon's burst;
// logs are dropped only when disk writes stall severely.
const LOG_CHANNEL_CAPACITY: usize = 512;

// ========== 宏（调用侧接口） / macros (caller API) ==========

/// 调试细节(每次事件/枚举/排序等),仅 Debug 档输出。
/// Diagnostic detail (per-event / enumeration / sorting), emitted only at the Debug tier.
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Debug, format_args!($($arg)*))
    };
}
/// 常规运行信息(启动/开关/菜单/错误提示等),Debug 与 Info 档均输出。
/// Normal runtime info (startup / toggles / menu / error notices), emitted at both tiers.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Info, format_args!($($arg)*))
    };
}

/// 被宏调用的核心函数：阈值过滤 + 拼时间戳 + 发送到 channel。
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
        // 有界通道:满时丢弃最新日志,绝不阻塞调用线程(日志不应拖慢 UI/事件循环)。
        // Bounded channel: drop the newest entry when full; never block the caller
        // (logging must not stall the UI / event loop).
        let _ = tx.try_send(msg);
    }
}

// ========== 生命周期 / lifecycle ==========

/// 在主线程早期调用，启动后台 writer 线程。
/// Call early on the main thread to start the background writer thread.
/// `is_dev`: cargo run → stdout; 打包 .app → 文件 / packaged .app → file.
pub fn init(config: &LogConfig, is_dev: bool) {
    let (tx, rx) = flume::bounded::<String>(LOG_CHANNEL_CAPACITY);
    LOG_TX.set(tx).ok();
    LOG_LEVEL.store(config.level as usize, Ordering::Relaxed);

    let file_path = resolve_file_path(config);
    std::thread::spawn(move || writer_loop(rx, is_dev, file_path));
    // stderr 重定向到日志管线:NSLog/AppKit 警告(如 Menu_Tracking 内部消息)、
    // panic 等系统输出都走 stderr,不捕获就会漏掉(只出现在终端/统一日志里)。
    // Redirect stderr into the log pipeline: system output like NSLog/AppKit warnings
    // (e.g. Menu_Tracking internals) and panics go through stderr; without capture they
    // are invisible in our log (only the terminal / unified log sees them).
    capture_stderr();
}

/// 运行时调整日志级别（reload_config 触发）。
/// Runtime log level adjustment (triggered by reload_config).
pub fn reconfigure(level: LogLevel) {
    LOG_LEVEL.store(level as usize, Ordering::Relaxed);
}

// ========== 内部实现 / internals ==========

fn writer_loop(rx: Receiver<String>, is_dev: bool, file_path: Option<String>) {
    let mut file: Option<BufWriter<std::fs::File>> = file_path
        .and_then(|path| {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| {
                    // writer 线程自身出问题时降级到 stderr / fallback to stderr on writer thread failure
                    eprintln!("[logger] cannot open log file {}: {}", path, e);
                })
                .ok()
        })
        .map(BufWriter::new);

    while let Ok(msg) = rx.recv() {
        if is_dev {
            print!("{}", msg); // stdout,msg 已含 \n / msg already contains \n
        }
        if let Some(ref mut f) = file {
            let _ = f.write_all(msg.as_bytes());
            // 每条 flush:崩溃时最多丢 1 条,文件不损坏 / flush each line: at most 1 msg lost on crash
            let _ = f.flush();
        }
    }
}

// ========== stderr 捕获(NSLog/AppKit 系统输出) / stderr capture ==========

extern "C" {
    fn pipe(fds: *mut c_int) -> c_int;
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut std::ffi::c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
}

/// 把进程的 stderr(fd 2)重定向到管道,读线程按行转交给日志管线(Info 级)。
/// NSLog 的可见输出、AppKit 内部警告、Rust panic 都写 stderr;重定向后这些消息
/// 带 `[stderr]` 前缀进入正常日志格式(时间戳 + INFO),与自家宏统一,不再只
/// 出现在终端。dev 模式下 writer_loop 仍会打到 stdout,终端照常可见。
///
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
        std::thread::spawn(move || stderr_reader(read_fd));
    }
}

/// 读线程:阻塞读管道,按 \n 切行(半行缓冲),每行去尾 \r 后以 Info 级输出。
/// 管道写满时线程阻塞在 read,writer 线程持续排空,不会撑爆内存。
///
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
    // EOF(进程退出前理论上到不了):冲刷残留的半行。
    // EOF (unreachable before process exit in practice): flush any trailing partial line.
    if !pending.is_empty() {
        emit_stderr_line(&pending);
    }
    unsafe {
        let _ = close(fd);
    }
}

fn emit_stderr_line(line: &[u8]) {
    // NSLog 行尾可能是 \r(老式行尾),只去掉 \r,保留其余内容。
    // NSLog lines can end with \r (legacy line ending); strip only the \r.
    let trimmed = if line.last() == Some(&b'\r') {
        &line[..line.len() - 1]
    } else {
        line
    };
    let s = String::from_utf8_lossy(trimmed).into_owned();
    _log(LogLevel::Info, format_args!("[stderr] {}", s));
}

fn resolve_file_path(config: &LogConfig) -> Option<String> {
    // dev 模式也写文件:writer_loop 在 is_dev 时同时输出到 stdout 与文件,
    // 便于开发时日志持久化(终端关闭不丢)。30 天清理同样作用于 dev 日志。
    // Dev mode also writes to a file: writer_loop prints to stdout AND the file when is_dev,
    // so dev logs persist (not lost when the terminal closes). The 30-day cleanup applies
    // to dev logs too.
    // 用户自定义路径:原样使用(append 模式,不加时间戳、不做清理,由用户自行管理轮转)。
    // 我们不会往用户指定的位置写入额外文件,也不会删除其中的任何文件。
    // User-supplied path: use verbatim (append mode, no timestamp, no cleanup - the user
    // manages rotation themselves). We never write extra files into, or delete files from,
    // a user-specified location.
    if !config.file_path.is_empty() {
        return Some(config.file_path.clone());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!("{}/Library/Logs/oh-my-tab", home);
    let _ = std::fs::create_dir_all(&dir);
    // 启动时清理 30 天前的旧日志(仅默认目录、仅 oh-my-tab-*.log)。
    // Prune logs older than 30 days at startup (default dir only, oh-my-tab-*.log only).
    cleanup_old_logs(&dir);
    let stamp = file_timestamp();
    Some(format!("{}/oh-my-tab-{}.log", dir, stamp))
}

/// 删除日志目录中修改时间超过 30 天的 oh-my-tab-*.log 文件。
/// 仅按 mtime 判断;正在写入的当前文件 mtime 持续更新,不会被误删。
/// 仅作用于默认日志目录,不会触碰用户自定义路径所在的目录。
///
/// Delete oh-my-tab-*.log files in the log dir whose mtime is older than 30 days.
/// Judged by mtime only; the current file's mtime keeps updating as we write, so it's never pruned.
/// Only the default log dir is touched - never the directory of a user-supplied path.
fn cleanup_old_logs(dir: &str) {
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
        // 只清理本应用产生的日志,避免误删同目录下的其他文件。
        // Only touch our own logs; never delete unrelated files in the same dir.
        if !name.starts_with("oh-my-tab-") || !name.ends_with(".log") {
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

// ========== 时间戳 / timestamp ==========

type TimeT = i64;

#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
    tm_gmtoff: i64,
    tm_zone: *const i8,
}

extern "C" {
    fn localtime_r(time: *const TimeT, result: *mut Tm) -> *mut Tm;
}

/// 零依赖 ISO-8601 时间戳，精确到毫秒。格式："2025-07-25T17:08:30.123"
/// Zero-dep ISO-8601 timestamp with milliseconds.
fn now_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let ms = now.subsec_millis();
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = secs as TimeT;
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

/// 启动时间戳(文件名安全,无冒号),用于给本次运行的日志文件命名。
/// Startup timestamp (filename-safe, no colons) used to name this run's log file.
fn file_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = secs as TimeT;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        )
    }
}
