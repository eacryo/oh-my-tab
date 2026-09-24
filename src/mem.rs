//! Memory sampler: a background thread periodically logs this process's memory usage
//! (system metrics + app-side ledgers) so leaks and regressions leave a data trail.
//! Info level, one sample every 5 minutes, with a ~60s-after-launch baseline. Purely
//! read-only sampling: no AppKit, all locks held only momentarily.

use crate::clipboard;
use crate::ffi::{bundle_info_string, task_vm_info, TaskVmInfo};
use crate::thumbnail;
use crate::{log_debug, log_info, CONFIG, WINDOW_COUNT};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Sampling interval. Even a leak as slow as 100KB/5min (~28MB/day) is visible in the
/// trend; sampling denser only dilutes the signal.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Delay before the baseline sample: leave startup work such as icon prewarming and
/// thumbnail generation time to settle, giving an approximate split between "allocated at
/// startup" and "growth during runtime". This is not a strict prewarming-complete barrier.
const BASELINE_DELAY: Duration = Duration::from_secs(60);

// The sampler tracks the footprint peak from thread startup; the kernel separately tracks
// the RSS peak. Footprint spikes between samples remain invisible by design.
static PEAK_FOOTPRINT: AtomicU64 = AtomicU64::new(0);

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

/// Thread count: this codebase spawns threads on demand (window-refresh / activation focus /
/// thumbnail observer / ax-raiser); a thread leak is likelier than a memory leak and shows
/// up earlier, so it is tracked alongside the footprint.
fn thread_count() -> u64 {
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

/// App-side ledger readings: item counts and bytes for the three big structures (thumbnail
/// LRU / clipboard history / window list). System metrics say "it grew"; ledgers say "who
/// grew" -- both print on one line for direct comparison.
struct Ledger {
    thumbs_items: usize,
    thumbs_bytes: u64,
    clip_entries: usize,
    clip_bytes: u64,
    clip_text_bytes: u64,
    clip_preview_bytes: u64,
    clip_metadata_bytes: u64,
    windows: usize,
}

fn read_ledgers() -> Ledger {
    let (thumbs_items, thumbs_bytes) = thumbnail::cache_stats();
    let clip = clipboard::history_stats();
    let windows = WINDOW_COUNT.load(Ordering::Acquire);
    Ledger {
        thumbs_items,
        thumbs_bytes,
        clip_entries: clip.entries,
        clip_bytes: clip.resident_bytes,
        clip_text_bytes: clip.text_bytes,
        clip_preview_bytes: clip.preview_bytes,
        clip_metadata_bytes: clip.metadata_bytes,
        windows,
    }
}

/// Record only feature switches that affect the memory profile; never record user content.
fn runtime_profile() -> String {
    let config = CONFIG.read().unwrap();
    let clipboard_mode = if !config.clipboard.enabled {
        "off"
    } else if config.clipboard.persist {
        "persistent"
    } else {
        "memory"
    };
    format!(
        "mouse:{},thumbs:{},clipboard:{}",
        if config.mouse.enabled { "on" } else { "off" },
        if config.layout.thumbnails_enabled {
            "on"
        } else {
            "off"
        },
        clipboard_mode,
    )
}

/// Sample once and log one line. A failed vminfo read (kernel interface change) only skips
/// this tick; the loop continues.
fn sample_once(started_at: Instant) {
    let Some(vm) = task_vm_info() else {
        log_debug!("[mem] sample skipped: task_vm_info unavailable");
        return;
    };
    let peak = track_peak(&vm);
    let ledger = read_ledgers();
    let profile = runtime_profile();
    let build = unsafe { bundle_info_string("CFBundleVersion") };
    log_info!(
        "[mem] pid={} build={} uptime={} phase=steady profile={} footprint={} rss={} footprint_peak_sampled={} rss_peak_kernel={} anon={} compressed={} threads={} | thumbs={{items={},ledger={}}} clipboard={{entries={},ledger={},text={},preview={},meta={}}} windows={{count={}}}",
        std::process::id(),
        build,
        fmt_uptime(started_at.elapsed()),
        profile,
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
        fmt_bytes(ledger.clip_text_bytes),
        fmt_bytes(ledger.clip_preview_bytes),
        fmt_bytes(ledger.clip_metadata_bytes),
        ledger.windows,
    );
}

/// Event memory snapshot: called only at thumbnail-batch/cache-clear boundaries so the
/// normal sampler frequency stays unchanged.
pub(crate) fn log_debug_snapshot(context: &str) {
    let Some(vm) = task_vm_info() else {
        log_debug!(
            "[mem] event={} snapshot skipped: task_vm_info unavailable",
            context
        );
        return;
    };
    let peak = track_peak(&vm);
    let ledger = read_ledgers();
    let profile = runtime_profile();
    let build = unsafe { bundle_info_string("CFBundleVersion") };
    log_debug!(
        "[mem] pid={} build={} event={} profile={} footprint={} rss={} footprint_peak_sampled={} rss_peak_kernel={} anon={} compressed={} threads={} | thumbs={{items={},ledger={}}} clipboard={{entries={},ledger={},text={},preview={},meta={}}} windows={{count={}}}",
        std::process::id(),
        build,
        context,
        profile,
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
        fmt_bytes(ledger.clip_text_bytes),
        fmt_bytes(ledger.clip_preview_bytes),
        fmt_bytes(ledger.clip_metadata_bytes),
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

/// Start the sampler thread (called once from main).
pub(crate) fn start() {
    thread::Builder::new()
        .name("mem-sampler".into())
        .spawn(|| {
            crate::performance::set_current_thread_qos(crate::performance::ThreadQos::Background);
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
        // A falling footprint must not lower the peak.
        assert_eq!(track_peak(&TaskVmInfo::with_footprint(50)), 100);
        assert_eq!(track_peak(&TaskVmInfo::with_footprint(300)), 300);
    }

    #[test]
    fn task_vm_info_reads_real_process() {
        // Smoke: one real read of this process; key fields should be non-zero and sane
        // (at least 1MB).
        let vm = task_vm_info().expect("task_vm_info should succeed for self");
        assert!(vm.phys_footprint > 1024 * 1024);
        assert!(vm.resident_size > 0);
    }
}
