use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::SystemTime;
use flume::{Receiver, Sender};

// ========== 日志级别 / log level ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub enum LogLevel {
    Info = 0,
    Warn = 1,
    Error = 2,
}

impl LogLevel {
    fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
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

// ========== 宏（调用侧接口） / macros (caller API) ==========

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Info, format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Warn, format_args!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logger::_log($crate::logger::LogLevel::Error, format_args!($($arg)*))
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
        let _ = tx.send(msg); // 非阻塞 / non-blocking
    }
}

// ========== 生命周期 / lifecycle ==========

/// 在主线程早期调用，启动后台 writer 线程。
/// Call early on the main thread to start the background writer thread.
/// `is_dev`: cargo run → stdout; 打包 .app → 文件 / packaged .app → file.
pub fn init(config: &LogConfig, is_dev: bool) {
    let (tx, rx) = flume::unbounded::<String>();
    LOG_TX.set(tx).ok();
    LOG_LEVEL.store(config.level as usize, Ordering::Relaxed);

    let file_path = resolve_file_path(config, is_dev);
    std::thread::spawn(move || writer_loop(rx, is_dev, file_path));
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

fn resolve_file_path(config: &LogConfig, is_dev: bool) -> Option<String> {
    if is_dev {
        return None;
    }
    if !config.file_path.is_empty() {
        return Some(config.file_path.clone());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = format!("{}/Library/Logs/oh-my-tab", home);
    let _ = std::fs::create_dir_all(&dir);
    Some(format!("{}/oh-my-tab.log", dir))
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
