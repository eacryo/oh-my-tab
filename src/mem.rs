//! 内存采样器:后台线程定期把本进程内存占用(system 指标 + 业务账本)打进日志,
//! 为泄漏排查与内存优化留下长期数据。Info 级、5 分钟一拍、启动后 ~60s 先打一条基线。
//! 纯只读采样:不碰 AppKit,锁都是瞬时获取。
//!
//! Memory sampler: a background thread periodically logs this process's memory usage
//! (system metrics + app-side ledgers) so leaks and regressions leave a data trail.
//! Info level, one sample every 5 minutes, with a ~60s-after-launch baseline. Purely
//! read-only sampling: no AppKit, all locks held only momentarily.

use crate::clipboard;
use crate::ffi::{task_vm_info, TaskVmInfo};
use crate::thumbnail;
use crate::{log_info, TAB_STATE};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// 采样周期。慢到 100KB/5min 的泄漏(≈28MB/天)也能从趋势中识别,更密只稀释信号。
/// Sampling interval. Even a leak as slow as 100KB/5min (~28MB/day) is visible in the
/// trend; sampling denser only dilutes the signal.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// 首条基线的延迟:给启动阶段的图标预热、缩略图预生成等工作留出缓冲时间,
/// 以便大致区分「启动即分配」与「运行期增长」;它不是严格的预热完成屏障。
/// Delay before the baseline sample: leave startup work such as icon prewarming and
/// thumbnail generation time to settle, giving an approximate split between "allocated at
/// startup" and "growth during runtime". This is not a strict prewarming-complete barrier.
const BASELINE_DELAY: Duration = Duration::from_secs(60);

// 采样器自己维护从线程启动开始的 footprint 峰值;系统 kernel 另外维护 RSS 峰值。
// 采样间隙的 footprint 尖峰无法捕捉,这是采样粒度的固有边界。
// The sampler tracks the footprint peak from thread startup; the kernel separately tracks
// the RSS peak. Footprint spikes between samples remain invisible by design.
static PEAK_FOOTPRINT: AtomicU64 = AtomicU64::new(0);

/// 格式化字节数:M/G 两位小数;小于 1MB 的记为 KB,避免 "0.0M" 掩盖小数值。
/// Format bytes: M/G with two decimals; sub-MB values print as KB so "0.0M" can't hide
/// small numbers.
fn fmt_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.2}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2}M", bytes as f64 / MB as f64)
    } else {
        format!("{}K", bytes / KB)
    }
}

/// 线程数:本代码库大量按需 spawn 线程(window-refresh / activation focus / thumbnail
/// observer / ax-raiser),线程泄漏比内存泄漏更可能发生且先暴露,和 footprint 并列监控。
/// Thread count: this codebase spawns threads on demand (window-refresh / activation focus /
/// thumbnail observer / ax-raiser); a thread leak is likelier than a memory leak and shows
/// up earlier, so it is tracked alongside the footprint.
fn thread_count() -> u64 {
    // /proc 不存在于 macOS;libproc 的 proc_listallpids 可数,但按进程名过滤本进程
    // 不可靠。最轻量的可靠口径:mach 的 task_threads 计数——直接 FFI,返回线程数组大小。
    // /proc doesn't exist on macOS; counting via libproc needs name matching. The lightest
    // reliable source is the mach task_threads array length -- direct FFI below.
    extern "C" {
        fn mach_task_self() -> u32;
        fn task_threads(target: u32, thread_list: *mut *mut u32, count: *mut u32) -> i32;
        fn vm_deallocate(target: u32, address: *mut u32, size: u32) -> i32;
    }
    unsafe {
        let mut threads: *mut u32 = std::ptr::null_mut();
        let mut count: u32 = 0;
        if task_threads(mach_task_self(), &mut threads, &mut count) != 0 {
            return 0;
        }
        if !threads.is_null() && count > 0 {
            // 数组由调用方负责释放(mach port 数组,按 count 个 mach_port_t 计)。
            // The array is caller-owned and must be deallocated (count mach_port_t entries).
            vm_deallocate(
                mach_task_self(),
                threads,
                count * std::mem::size_of::<u32>() as u32,
            );
        }
        count as u64
    }
}

/// 业务账本读数:三个大结构(缩略图 LRU / 剪贴板历史 / 窗口列表)的条目数与字节。
/// 系统指标只能说明「涨了」,账本才能说明「是谁涨的」;两者同行打印就是为了对照。
/// App-side ledger readings: item counts and bytes for the three big structures (thumbnail
/// LRU / clipboard history / window list). System metrics say "it grew"; ledgers say "who
/// grew" -- both print on one line for direct comparison.
struct Ledger {
    thumbs_items: usize,
    thumbs_bytes: u64,
    clip_entries: usize,
    clip_bytes: u64,
    windows: usize,
}

fn read_ledgers() -> Ledger {
    let (thumbs_items, thumbs_bytes) = thumbnail::cache_stats();
    let (clip_entries, clip_bytes) = clipboard::history_stats();
    let windows = TAB_STATE
        .lock()
        .unwrap()
        .as_ref()
        .map(|state| state.windows.len())
        .unwrap_or(0);
    Ledger {
        thumbs_items,
        thumbs_bytes,
        clip_entries,
        clip_bytes,
        windows,
    }
}

/// 采样并打一行日志。vminfo 读取失败(kernel 接口变化)只跳过本拍,不中断循环。
/// Sample once and log one line. A failed vminfo read (kernel interface change) only skips
/// this tick; the loop continues.
fn sample_once(started_at: Instant) {
    let Some(vm) = task_vm_info() else {
        log_info!("[mem] sample skipped: task_vm_info unavailable");
        return;
    };
    let peak = track_peak(&vm);
    let ledger = read_ledgers();
    log_info!(
        "[mem] uptime={} footprint={} rss={} footprint_peak_sampled={} rss_peak_kernel={} anon={} compressed={} threads={} | thumbs={} items/{} clip={} entries/{} windows={}",
        fmt_uptime(started_at.elapsed()),
        fmt_bytes(vm.phys_footprint),
        fmt_bytes(vm.resident_size),
        fmt_bytes(peak),
        fmt_bytes(vm.resident_size_peak),
        fmt_bytes(vm.internal),
        fmt_bytes(vm.compressed),
        thread_count(),
        ledger.thumbs_items,
        fmt_bytes(ledger.thumbs_bytes),
        ledger.clip_entries,
        fmt_bytes(ledger.clip_bytes),
        ledger.windows,
    );
}

fn track_peak(vm: &TaskVmInfo) -> u64 {
    let current = vm.phys_footprint;
    let mut peak = PEAK_FOOTPRINT.load(Ordering::Relaxed);
    while current > peak {
        match PEAK_FOOTPRINT.compare_exchange_weak(
            peak,
            current,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return current,
            Err(actual) => peak = actual,
        }
    }
    peak
}

fn fmt_uptime(elapsed: Duration) -> String {
    let mins = elapsed.as_secs() / 60;
    if mins < 60 {
        format!("{mins}m")
    } else {
        format!("{}h{}m", mins / 60, mins % 60)
    }
}

/// 启动采样线程(main 里一次性调用)。
/// Start the sampler thread (called once from main).
pub(crate) fn start() {
    thread::Builder::new()
        .name("mem-sampler".into())
        .spawn(|| {
            let started_at = Instant::now();
            // Seed the sampled footprint peak before the delayed baseline, so the peak
            // column does not silently ignore the first minute of initialization.
            if let Some(vm) = task_vm_info() {
                track_peak(&vm);
            }
            thread::sleep(BASELINE_DELAY);
            sample_once(started_at);
            loop {
                thread::sleep(SAMPLE_INTERVAL);
                sample_once(started_at);
            }
        })
        .expect("spawn mem-sampler thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting_avoids_hiding_small_values() {
        assert_eq!(fmt_bytes(0), "0K");
        assert_eq!(fmt_bytes(999 * 1024), "999K");
        assert_eq!(fmt_bytes(1024 * 1024), "1.00M");
        assert_eq!(fmt_bytes(64_000_000), "61.04M");
        assert_eq!(fmt_bytes(3 * 1024 * 1024 * 1024), "3.00G");
    }

    #[test]
    fn uptime_format_switches_to_hours_after_an_hour() {
        assert_eq!(fmt_uptime(Duration::from_secs(59 * 60)), "59m");
        assert_eq!(fmt_uptime(Duration::from_secs(83 * 60)), "1h23m");
    }

    #[test]
    fn peak_tracking_is_monotonic() {
        PEAK_FOOTPRINT.store(0, Ordering::Relaxed);
        assert_eq!(track_peak(&TaskVmInfo::with_footprint(100)), 100);
        // footprint 回落不拉低峰值。
        // A falling footprint must not lower the peak.
        assert_eq!(track_peak(&TaskVmInfo::with_footprint(50)), 100);
        assert_eq!(track_peak(&TaskVmInfo::with_footprint(300)), 300);
    }

    #[test]
    fn task_vm_info_reads_real_process() {
        // 冒烟:真实读一次本进程,关键字段应为非零、量级合理(≥1MB)。
        // Smoke: one real read of this process; key fields should be non-zero and sane
        // (at least 1MB).
        let vm = task_vm_info().expect("task_vm_info should succeed for self");
        assert!(vm.phys_footprint > 1024 * 1024);
        assert!(vm.resident_size > 0);
    }
}
