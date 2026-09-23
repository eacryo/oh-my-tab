//! 缩略图 · blank_frame:后台挂起 WKWebView 窗口的空白帧检测。
//! Blank-frame detection for background-suspended WKWebView windows.

use super::*;

// ========== 空白帧检测(后台挂起的 WKWebView 窗口) ==========
// WKWebView(Tauri/Electron/wry 等)的页面由独立 WebContent 进程渲染;窗口长时间
// 后台/被遮挡后 macOS 挂起该进程并丢弃 WindowServer 侧的内容表面,此时截窗口
// 只剩宿主进程绘制的标题栏(红绿灯),内容区域退化为逐像素一致的纯色(通常白)。
// macOS 没有任何公开 API 能强制别的进程重渲染,可行策略只有:
// 前台时捕获 + 保留最后一张有效帧,避免被空白帧覆盖。
// 本模块在缓存写入前对每帧做空白判定:
// - 后台窗口 + 已有缓存帧:丢弃,保住最后一张有效帧(升级单向)
// - 后台窗口 + 缓存为空:入缓存作为占位种子(之后前台补拍自动升级)
// - 前台窗口:如实入缓存(用户眼前就是空白画面)
// - 激活补拍仍空白:丢弃并延迟重拍一次(Web 内容尚未恢复重绘完)
// - 外观切换重拍:空白也覆盖(旧外观帧与新主题不协调比占位更刺眼)
//
// Blank-frame detection for background-suspended WKWebView windows: the page of a
// WKWebView-based app (Tauri/Electron/wry) renders in a separate WebContent
// process; once the window stays backgrounded/occluded, macOS suspends it and drops
// the WindowServer-side content surface, so captures degrade to the host-drawn
// title bar (traffic lights) over a pixel-uniform solid body (usually white). No
// public API can force another process to redraw; the answer adopted here is to
// capture while frontmost and never let a blank frame overwrite the
// last-known-good thumbnail. Before any
// cache write each frame is classified:
// - background + cached frame: dropped, keeping the last-known-good image (one-way)
// - background + empty cache: stored as a placeholder seed (frontmost captures
//   upgrade it automatically afterwards)
// - frontmost: stored as-is (the user is literally looking at it)
// - still blank on an activation refresh: dropped with one delayed retry
// - appearance-transition recapture: blank overwrites too (a stale-appearance
//   frame amid the new theme looks worse than a placeholder)

/// Device RGB 色彩空间不可变且线程安全,进程级复用;降采样与空白判定共用。
/// The Device RGB color space is immutable and thread-safe; one process-wide
/// instance is shared by downscaling and blank analysis.
pub(super) static DEVICE_RGB_COLOR_SPACE: LazyLock<RetainedCf<c_void>> =
    LazyLock::new(|| unsafe { RetainedCf::from_retained(CGColorSpaceCreateDeviceRGB()) });

/// 空白判定的采样最长边:把帧重绘到 ≤64px 的 RGBA 小位图再统计,单帧开销微秒级。
/// Sampling longest edge for blank analysis: the frame is redrawn into a small
/// (<=64px) RGBA bitmap first; per-frame cost stays in the microsecond range.
const BLANK_SAMPLE_MAX_DIM: u32 = 64;
/// 内容区从标题栏/工具条之下开始统计:挂起 WebView 的标题栏由宿主进程绘制,仍是
/// 正常画面,必须排除在"内容近纯色"判定外。28pt 标题栏在 400~1200pt 高的窗口中
/// 占 2.3%~7%,取 12% 覆盖标题栏加常见工具条。
/// Content rows start below the title bar / toolbar strip: a suspended WebView's
/// title bar is host-drawn and still renders normally, so it must be excluded from
/// the near-uniform test. A 28pt title bar spans 2.3%~7% of 400~1200pt-tall windows;
/// 12% covers the bar plus a common toolbar.
const BLANK_TITLE_STRIP_FRACTION: f64 = 0.12;
/// 单一颜色桶覆盖率 ≥99% 判为空白:挂起 WebView 的内容区逐像素一致,覆盖率≈1.0;
/// 真实 UI(侧栏/文本/控件/边框)远达不到 99%。桶按通道 5bit 量化,轻微压缩
/// 噪声不会造成假阴性。
/// A single quantized color bucket covering >=99% of content rows classifies as
/// blank: suspended WebView bodies are pixel-uniform (coverage ~1.0) while real UIs
/// (sidebars/text/controls/borders) never approach 99%. Buckets quantize channels
/// to 5 bits so mild compression noise cannot fake a negative.
pub(super) const BLANK_MODAL_COVERAGE_MIN: f64 = 0.99;
/// 激活补拍遇到空白帧后的重试延迟:给 WebContent 进程恢复并完成一轮重绘的时间
/// (首拍 350ms 仍白说明恢复偏慢,重试给到约 1.25s 总窗口)。
/// Retry delay after a blank activation refresh: gives the WebContent process time
/// to restore and finish a redraw pass (a blank frame at the initial 350ms means
/// restoration is slow; the retry lands at a ~1.25s total window).
pub(super) const ACTIVATION_BLANK_RETRY_MS: u64 = 900;

/// 已安排延迟重试的窗口键。名额在帧最终入库、任务以失败/失效终止、重试因失焦/换代
/// 放弃或 App 退出时释放;同一激活补拍链至多重试一次,防止空白-重拍死循环。
/// Window keys with a delayed retry scheduled. A slot is released when a frame is finally
/// stored, the task terminates unsuccessfully/stale, the retry is abandoned (focus lost /
/// generation changed), or the app terminates; each activation chain retries at most once.
pub(super) static PENDING_BLANK_RETRIES: LazyLock<Mutex<HashSet<ThumbKey>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// 内容区(跳过顶部标题条带后)单一颜色桶的覆盖率;None = 没有可统计像素。
/// 纯函数:输入 RGBA 字节流,便于单元测试。
/// Coverage of the single most common color bucket over the content rows (below the
/// title strip); None when there is nothing to measure. Pure function over an RGBA
/// byte buffer so it is unit-testable without CoreGraphics.
pub(super) fn blank_modal_coverage(
    rgba: &[u8],
    w: usize,
    skip_rows: usize,
    h: usize,
) -> Option<f64> {
    if w == 0 || h <= skip_rows || rgba.len() < w * h * 4 {
        return None;
    }
    let mut counts: HashMap<u16, usize> = HashMap::new();
    let mut total = 0usize;
    for y in skip_rows..h {
        let row = y * w * 4;
        for x in 0..w {
            let i = row + x * 4;
            let bucket = (((rgba[i] as u16) >> 3) << 10)
                | (((rgba[i + 1] as u16) >> 3) << 5)
                | ((rgba[i + 2] as u16) >> 3);
            *counts.entry(bucket).or_default() += 1;
            total += 1;
        }
    }
    let modal = counts.values().copied().max()?;
    Some(modal as f64 / total as f64)
}

/// 空白帧的处理决策(纯函数,便于矩阵化测试)。
/// What to do with a blank frame (pure function for matrix testing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BlankFrameAction {
    /// 前台窗口的空白是用户眼前的真实画面,如实入缓存。
    /// A blank FRONTMOST window is what the user literally sees; store it.
    Store,
    /// 后台窗口的首帧(缓存为空):入缓存作为占位种子。挂起 WebView 只能截到
    /// "标题栏+纯色内容",但一张占位帧好过图标卡——之后任何一次前台补拍
    /// (激活/前台召唤)都会把它升级成真实画面,而门控保证升级不可逆。
    /// The FIRST frame of a background window (empty cache): store as a placeholder
    /// seed. A suspended WebView only yields title bar + solid body, yet a
    /// placeholder beats an icon card -- any later frontmost capture (activation /
    /// frontmost summon) upgrades it to the real page, and the gating makes that
    /// upgrade one-way.
    StoreSeed,
    /// 外观(明暗主题)切换的重拍:空白帧也覆盖。旧帧是旧外观像素,与其他卡片
    /// 不一致比暂时空白更刺眼;真实画面在下次前台时自然补回。
    /// Appearance (light/dark) transition recapture: blank overwrites too. The old
    /// frame carries stale-appearance pixels that clash with every other card worse
    /// than a temporary blank; the real page returns on the next frontmost capture.
    StoreAppearanceRefresh,
    /// 后台窗口的空白帧 = 挂起 WebView;丢弃,保住缓存里的最后一张有效帧。
    /// A blank BACKGROUND frame means a suspended WebView; drop it and keep the
    /// cached last-known-good image.
    DiscardKeepLastGood,
    /// 前台激活补拍仍空白 = Web 内容尚未重绘完成;丢弃并安排一次延迟重拍。
    /// A blank frame on an activation refresh means Web content has not repainted
    /// yet; drop it and schedule one delayed recapture.
    DiscardRetryActivation,
}

pub(super) fn blank_frame_action(
    frontmost: bool,
    activation_job: bool,
    cache_has_frame: bool,
    retry_slot_acquired: bool,
    appearance_refresh: bool,
) -> BlankFrameAction {
    // 激活补拍的空白重试优先于外观覆盖:重试拍到的真实帧同样满足外观一致性,
    // 而直接入库会跳过 1.4s 兜底重试,把"还没画完"的画面定格成占位帧(主题任务
    // 与激活请求合并时两个标志会同帧出现,必须在此处分出先后)。
    // The activation blank retry outranks the appearance overwrite: the retried
    // real frame satisfies appearance consistency too, while storing now would
    // skip the 1.4s backstop retry and freeze an unfinished repaint as the
    // placeholder (a theme job merged with an activation request carries both
    // flags on one frame, so the order must be settled here).
    if frontmost && activation_job && cache_has_frame && retry_slot_acquired {
        return BlankFrameAction::DiscardRetryActivation;
    }
    if appearance_refresh {
        // 外观一致性优先于内容保真:即使前台空白会走 Store,后台空白也会覆盖
        // 有效帧,统一由本分支放行。
        // Appearance consistency wins over content fidelity: blank frames store for
        // both the frontmost and background cases through this single branch.
        return BlankFrameAction::StoreAppearanceRefresh;
    }
    if !frontmost {
        return if cache_has_frame {
            BlankFrameAction::DiscardKeepLastGood
        } else {
            BlankFrameAction::StoreSeed
        };
    }
    BlankFrameAction::Store
}

/// 把帧重绘进 ≤64px 的 RGBA 位图后做空白判定;分析不可用时返回 None,调用方按
/// 非空白处理(保守起见维持原入缓存行为)。
/// Redraw the frame into a small (<=64px) RGBA bitmap and run the blank test.
/// Analysis failures return None and the caller treats the frame as non-blank
/// (conservatively preserving the original store behavior).
pub(super) unsafe fn frame_blankness(img: *const c_void, w_px: u32, h_px: u32) -> Option<bool> {
    if img.is_null() || w_px == 0 || h_px == 0 {
        return None;
    }
    let scale = BLANK_SAMPLE_MAX_DIM as f64 / u32::max(w_px, h_px) as f64;
    let sw = (((w_px as f64) * scale).round() as usize).max(1);
    let sh = (((h_px as f64) * scale).round() as usize).max(1);
    let ctx = CGBitmapContextCreate(
        std::ptr::null_mut(),
        sw,
        sh,
        8,
        sw * 4,
        DEVICE_RGB_COLOR_SPACE.ptr,
        BITMAP_PREMULTIPLIED_LAST,
    );
    if ctx.is_null() {
        return None;
    }
    CGContextDrawImage(
        ctx,
        CGRect {
            x: 0.0,
            y: 0.0,
            w: sw as f64,
            h: sh as f64,
        },
        img,
    );
    let data = CGBitmapContextGetData(ctx) as *const u8;
    let coverage = if data.is_null() {
        None
    } else {
        let skip_rows = ((sh as f64) * BLANK_TITLE_STRIP_FRACTION).round() as usize;
        blank_modal_coverage(
            std::slice::from_raw_parts(data, sw * sh * 4),
            sw,
            skip_rows,
            sh,
        )
    };
    CFRelease(ctx);
    Some(coverage.is_some_and(|c| c >= BLANK_MODAL_COVERAGE_MIN))
}

/// App 退出时一并丢弃其挂起的空白重试名额(pregen 的终止路径调用)。
/// Drop a terminated app's pending blank-retry slots (called from pregen's
/// termination path).
pub(crate) fn forget_blank_retries_for_pid(pid: i32) {
    PENDING_BLANK_RETRIES
        .lock()
        .unwrap()
        .retain(|key| key.pid != pid);
}
