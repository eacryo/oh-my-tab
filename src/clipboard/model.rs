//! 剪贴板子系统 · model:数
//! 据

use super::*;

// ========== 纯逻辑(可测)/ pure logic (testable) ==========

/// 按显示宽度估算文本的换行行数:ASCII 字符记 1 单位,中文/全角记 2;显式换行符单独成行。
/// Estimate the wrapped line count by display width: ASCII counts as 1 unit, CJK/full-width
/// as 2; explicit newlines start a new line.
pub(super) fn estimate_lines(text: &str, max_units: usize) -> usize {
    let mut units = 0usize;
    let mut lines = 1usize;
    for ch in text.chars() {
        if ch == '\n' {
            lines += 1;
            units = 0;
            continue;
        }
        let w = if ch.is_ascii() { 1 } else { 2 };
        if units + w > max_units {
            lines += 1;
            units = w;
        } else {
            units += w;
        }
    }
    lines
}

/// 把文本截断为最多 max_lines 行(按显示宽度),截断处(第 max_lines 行末尾)加省略号。
/// Truncate the text to at most `max_lines` display lines (by width), appending an ellipsis
/// at the truncation point (the end of the last kept line).
pub(super) fn truncate_to_lines(text: &str, max_units: usize, max_lines: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    let mut lines = 1usize;
    for ch in text.chars() {
        if ch == '\n' {
            if lines >= max_lines {
                out.push('…');
                return out;
            }
            lines += 1;
            units = 0;
            out.push('\n');
            continue;
        }
        let w = if ch.is_ascii() { 1 } else { 2 };
        if units + w > max_units {
            if lines >= max_lines {
                out.push('…');
                return out;
            }
            lines += 1;
            units = w;
            out.push(ch);
        } else {
            units += w;
            out.push(ch);
        }
    }
    out
}

/// 详情面板可用宽 → 每行可容纳的显示宽度单位,与行按钮同一估算口径
/// (50 单位 ≈ 行内容宽 ≈ 346pt)。
/// Detail-panel content width -> per-line width units, using the same estimate as the row
/// buttons (50 units fit the row content width ≈ 346pt).
pub(super) fn detail_text_units(width: f64) -> usize {
    let units_per_pt = LINE_MAX_UNITS as f64 / content_width();
    ((width * units_per_pt).floor() as usize).max(1)
}

/// 行内容可用宽度:窗口宽 - 两翼留白 - 来源图标 - 图标间隙 - 操作按钮条。
/// The row content's usable width: window - both paddings - icon - icon gap - actions.
pub(super) fn content_width() -> f64 {
    // 内容按钮宽:窗口 - 列表边距 - 行内左右内边距(新设计稿 padding 13/11)。
    // The content button's width: window - list margins - the row's L/R padding.
    PICKER_W - PAD_X * 2.0 - ROW_PAD_L - ROW_PAD_R
}

/// 是否显示来源应用(读 CONFIG;记录始终进行,开关同时控制行内副信息的名称和图标)。
/// Whether the source app is shown (reads CONFIG; recording is always on, the toggle gates
/// both the name and icon in the row's meta line).
pub(super) fn show_source_app() -> bool {
    CONFIG.read().unwrap().clipboard.show_source_app
}

/// 来源图标和来源名称属于同一个显示开关;缺少图标缓存键时也无需尝试读取文件。
/// The source icon and name share one display switch; without an icon-cache key there is no
/// file to load either.
pub(super) fn should_show_source_icon(show_source: bool, entry: &ClipEntry) -> bool {
    show_source && !entry.source_key.is_empty()
}

/// 行内副信息(应用名 · 相对时间):正文下方的小字,按设计稿 10px 浅灰。
/// 类型提示改由正文本身的着色/字体表达(URL 蓝、代码等宽),副信息不再挂角标。
/// The row's meta line (app · relative time): the small text below the content, 10px
/// light gray per the mockup. The kind cue moved INTO the content itself (blue URLs,
/// monospaced code); the meta line carries no badge.
pub(super) fn build_meta_text(entry: &ClipEntry, show_source: bool) -> String {
    let mut parts: Vec<String> = Vec::new();
    if show_source {
        if entry.source_app.is_empty() {
            parts.push(t("clipboard.unknown_source"));
        } else {
            parts.push(entry.source_app.clone());
        }
    }
    if let Some(ts) = entry.copied_at {
        parts.push(relative_time_label(ts, now_secs()));
    }
    parts.join(" · ")
}

/// 相对时间:刚刚 / 今天 HH:mm / 昨天 HH:mm / 更早 MM-dd HH:mm(本地时区)。
/// Relative time: Just now / Today HH:mm / Yesterday HH:mm / older MM-dd HH:mm (local tz).
pub(super) fn relative_time_label(ts: u64, now: u64) -> String {
    if now.saturating_sub(ts) < 60 {
        return t("clipboard.time_just_now");
    }
    let delta = day_no(ts) - day_no(now);
    let hhmm = local_hhmm(ts);
    match delta {
        0 => tf("clipboard.time_today", &[("time", &hhmm)]),
        1 => tf("clipboard.time_yesterday", &[("time", &hhmm)]),
        _ => format_copied_at(ts),
    }
}

/// 本地日期序号(年 × 366 + 年内第几天):同一天相减 = 0,昨天 = 1(与 DST 无关)。
/// The local day ordinal (year*366 + yday): same-day diff = 0, yesterday = 1 (DST-proof).
pub(super) fn day_no(unix_secs: u64) -> i64 {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        tm.tm_year as i64 * 366 + tm.tm_yday as i64
    }
}

/// 时间戳 → 本地 HH:mm / timestamp -> local HH:mm.
pub(super) fn local_hhmm(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

/// 时间分组:今天 / 昨天 / 更早(按本地日序号)。无时间戳的旧条目归入更早。
/// Time group: Today / Yesterday / Earlier (by the local day ordinal). Legacy entries
/// without a timestamp join Earlier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DayGroup {
    Today,
    Yesterday,
    Earlier,
}

pub(super) fn day_group(ts: Option<u64>) -> DayGroup {
    let Some(ts) = ts else {
        return DayGroup::Earlier;
    };
    match day_no(ts) - day_no(now_secs()) {
        0 => DayGroup::Today,
        1 => DayGroup::Yesterday,
        _ => DayGroup::Earlier,
    }
}

/// 分组头文字(轻量分隔)/ the group header's label.
pub(super) fn group_label(g: DayGroup) -> String {
    match g {
        DayGroup::Today => t("clipboard.group_today"),
        DayGroup::Yesterday => t("clipboard.group_yesterday"),
        DayGroup::Earlier => t("clipboard.group_earlier"),
    }
}

/// 行内容高度:统一固定 61pt(设计稿 min-height 61px,内容垂直居中)。
/// The row content height: uniformly 61pt (the mockup's min-height 61px, content
/// vertically centered).
pub(super) fn row_content_h(_entry: &ClipEntry) -> f64 {
    ROW_H
}

/// 计算一批条目的每行行距。**每条目标定同一行距** = 内容高 + 行距;分组边界
/// 前插入分组头高度(列表筛完后的显示顺序下,时间跨组必然正确)。
/// Compute the per-row pitches. **Every entry keeps ONE fixed pitch** = content + row gap;
/// a group-header height is inserted before the first row of each group (computed on the
/// FILTERED display order, so time boundaries stay correct under filtering).
pub(super) fn compute_pitches(texts: &[ClipEntry]) -> Vec<f64> {
    let mut prev: Option<DayGroup> = None;
    texts
        .iter()
        .map(|e| {
            let g = day_group(e.copied_at);
            let hdr = if prev.is_none() || prev != Some(g) {
                GROUP_H
            } else {
                0.0
            };
            prev = Some(g);
            hdr + row_content_h(e)
        })
        .collect()
}

/// 固定头部条高度:顶部留白 + 搜索框/筛选/清除按钮行 + 与列表的间距。
/// The fixed header strip's height: top padding + the search/filter/clear row + the gap
/// to the list.
/// 固定头部条高度:搜索栏区(14 + 48 + 8)+ 筛选行(38),镜像设计稿。
/// The fixed header strip: the search zone (14 + 48 + 8) + the filters row (38).
pub(super) fn header_strip_h() -> f64 {
    TOP_PAD_Y + SEARCH_H + SEARCH_GAP_Y + FILTERS_H
}

/// 文档内行列表的顶部偏移:仅保留与头部条的间距(头部条已不在滚动区内,
/// 不再需要为它让位 38pt——那会留下"第一条与搜索框之间的奇怪空白")。
/// The row list's top offset INSIDE the document: just the gap to the header strip (the
/// strip is no longer inside the scroll area, so no 38pt clearance is needed -- that left
/// the odd blank band between the first row and the search field).
pub(super) fn rows_top_offset() -> f64 {
    CLEAR_BTN_GAP
}

/// 第 idx 行的顶部 y(flipped 坐标):rows_top_offset + 前 idx 行行距之和。
/// The top y of row `idx` (flipped coords): rows_top_offset + the pitches before it.
pub(super) fn row_top(idx: usize, pitches: &[f64]) -> f64 {
    rows_top_offset() + pitches.iter().take(idx).sum::<f64>()
}

/// u64 以十六进制字符串序列化:TOML 整数是 i64,高位置位的 hash 直接序列化会失败
/// ("u64 value out of range"),哈希必须走字符串。
/// u64 serialized as a hex string: TOML integers are i64, so hashes with the high bit
/// set would fail to serialize ("u64 value out of range"); hashes must go as strings.
pub(super) mod u64_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{v:016x}"))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        u64::from_str_radix(&s, 16).map_err(serde::de::Error::custom)
    }
}

/// 图片条目,两种形态:
/// - **数据条目**(图片数据复制):原始格式字节的**磁盘引用** + UTI + 降采样 PNG
///   预览。原始字节不驻内存:录制时算 hash 写入缓存文件,粘贴时按需读回、按 `uti`
///   原样写回(JPG 粘回 JPG、GIF 动图粘回动图);预览只供缩略图(动图取第一帧)。
/// - **文件复制条目**(Finder 复制图片文件):复制时读一次文件内容(瞬时)算内容
///   hash + 生成缩略图预览,字节丢弃(data_path 恒空,无影子副本)。hash 用于同内容
///   去重(原文件与访达副本只留一条);`source_path` 记录来源。粘贴时恢复
///   `public.file-url`,把"文件"而不是"图片数据"交回给目标应用(Finder 复制原文件、
///   聊天应用附加文件,GIF 动画因此完整保留;若把图片数据当纯图片粘贴,Finder 会
///   直接忽略,部分应用还会重编码成 PNG);源文件被删/移动后该条目粘贴即失效。
///
/// An image entry, in two forms:
/// - a DATA entry (image-data copy): a DISK reference to the original-format bytes + its
///   UTI + a downsampled PNG preview. The original bytes never stay in memory: hashed and
///   written to a cache file at record time, read back on paste and written verbatim under
///   `uti` (a JPG pastes back as JPG, an animated GIF as a GIF); the preview is only for
///   the thumbnail (first frame for animations, never pasted).
/// - a FILE-COPY entry (an image file copied in Finder): the file is read ONCE at record
///   time (transiently) for a content hash + a thumbnail preview, then the bytes are
///   discarded (data_path is always empty, no shadow copy). The hash drives CONTENT dedup
///   (a file and its Finder duplicate collapse into one entry); `source_path` records the
///   source. Pasting restores `public.file-url`, handing the FILE (not the image data) to
///   the target app (Finder duplicates the original file, chat apps attach the file, GIF
///   animation fully preserved; bare image data is ignored by Finder and re-encoded into
///   PNG by some apps); a deleted/moved source makes the entry unpastable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ImageEntry {
    /// 原始格式的 UTI(public.png / public.jpeg / com.compuserve.gif ...)。
    /// The original format's UTI (public.png / public.jpeg / com.compuserve.gif ...).
    pub(super) uti: String,
    /// 原始字节的内容哈希(FNV-1a):数据条目 = 缓存文件名 + 查重键;文件复制条目 =
    /// 同内容去重键 + 预览文件名。解码失败的退化文件条目为 0。序列化为十六进制
    /// 字符串(TOML 整数放不下 64 位无符号)。
    /// The original bytes' content hash (FNV-1a): the cache filename + dedup key for data
    /// entries; the content-dedup key + preview filename for file-copy entries. 0 for a
    /// degenerate file entry whose decode failed. Serialized as a hex string (TOML
    /// integers can't hold 64-bit unsigned values).
    #[serde(with = "u64_hex")]
    pub(super) hash: u64,
    /// 原始格式字节的缓存文件路径(仅数据条目;文件复制条目恒空——粘贴走 file-url)。
    /// 序列化时跳过:加载时由 hash 重建。
    /// The cache file holding the original-format bytes (data entries only; always empty
    /// for file-copy entries -- pasting goes through the file-url). Skipped in
    /// serialization: rebuilt from the hash on load.
    #[serde(skip)]
    pub(super) data_path: std::path::PathBuf,
    /// 降采样 PNG 预览(缩略图绘制用;唯一常驻内存的图片字节,约 100-300KB)。
    /// 序列化时跳过:预览单独落盘为 `{hash}.preview`,加载时读回(缺失则从数据字节
    /// 或源文件重新生成)。
    /// A downsampled PNG preview (thumbnail drawing; the only image bytes held in memory,
    /// ~100-300KB). Skipped in serialization: the preview lives separately as
    /// `{hash}.preview` and is read back on load (regenerated from the data bytes or the
    /// source file when missing).
    #[serde(skip)]
    pub(super) preview_png: Vec<u8>,
    /// 文件复制的来源路径(None = 数据条目,纯图片复制)。
    /// The source path of a file copy (None = a data entry, a bare image copy).
    pub(super) source_path: Option<String>,
}

/// 历史条目:文本 + 置顶标记 + 来源应用名 + 来源图标缓存键。置顶条目恒在列表顶部。
/// A history entry: text + a pinned flag + the source app name + the source icon-cache key.
/// Pinned entries stay at the top.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct ClipEntry {
    pub(super) text: String,
    /// 图片条目(原始格式 + 预览;文本条目为 None)。图文同存时文本优先,
    /// 图片仅在剪贴板无文本时记录。
    /// An image entry (original format + preview; None for text entries). When both text
    /// and an image are on the pasteboard, text wins -- images are only recorded when
    /// there is no text.
    pub(super) image: Option<ImageEntry>,
    pub(super) pinned: bool,
    /// 复制该文本时的前台应用名(空 = 未知,如旧条目/取不到前台应用)。
    /// The frontmost app name when the text was copied (empty = unknown, e.g. legacy entries
    /// or an unavailable frontmost app).
    pub(super) source_app: String,
    /// 来源应用的图标缓存键(resolve_app_identity: bundle id > exec 路径哈希 > pid)。
    /// 空 = 取不到身份(旧条目等),标题栏不显示图标。
    /// The source app's icon-cache key (resolve_app_identity: bundle id > exec-path hash >
    /// pid). Empty = no identity (e.g. legacy entries) -> no icon in the header.
    pub(super) source_key: String,
    /// 复制时间戳(unix 秒):自动过期的依据,去重移前时刷新为最近一次复制时间。
    /// None = 旧版本条目(无时间戳),不参与过期——保守迁移,绝不误删。
    /// The copy timestamp (unix seconds): the basis of auto-expiry; refreshed to the
    /// latest copy time on dedup-move-to-front. None = a legacy entry (no timestamp),
    /// exempt from expiry -- a conservative migration, never wrongly deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) copied_at: Option<u64>,
}

/// 当前 unix 秒。/ The current unix seconds.
pub(super) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// localtime_r 与 Tm 已统一到 ffi.rs(与 logger 同一份声明)。
// localtime_r and Tm now live in ffi.rs (one declaration shared with the logger).

/// 复制时间戳 → "MM-dd HH:mm"(本地时区;标题栏空间有限,省略年份)。
/// 纯函数,单测覆盖格式。
/// Copy timestamp -> "MM-dd HH:mm" (local time; the header bar is narrow, so the year is
/// dropped). Pure function; the format is unit-tested.
pub(super) fn format_copied_at(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "{:02}-{:02} {:02}:{:02}",
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min
        )
    }
}

/// 复制时间戳 → "YYYY-MM-DD HH.MM.SS"(本地时区,另存为的建议文件名后缀)。
/// 时间部分刻意用点号而非冒号——冒号在 HFS+/Finder 里是非法/保留字符(macOS 截图
/// 同款命名风格)。纯函数,单测覆盖格式。
/// Copy timestamp -> "YYYY-MM-DD HH.MM.SS" (local time; the save-as suggested-filename
/// suffix). The time part deliberately uses dots instead of colons -- colons are
/// illegal/reserved in HFS+/Finder names (macOS screenshot naming style). Pure function;
/// the format is unit-tested.
pub(super) fn format_save_stamp(unix_secs: u64) -> String {
    unsafe {
        let mut tm: Tm = std::mem::zeroed();
        let s = unix_secs as i64;
        localtime_r(&s, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}.{:02}.{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// 另存为建议文件名的时间戳:取条目的复制时间(copied_at);旧版本条目无时间戳,
/// 退化为保存时刻(有总比无强,且旧条目会随过期策略淘汰)。
/// The save-as suggested-filename stamp: the entry's copy time (copied_at); legacy
/// entries without a timestamp degrade to the save moment (better than nothing, and
/// legacy entries age out via expiry anyway).
pub(super) fn save_stamp_for(entry: &ClipEntry) -> String {
    format_save_stamp(entry.copied_at.unwrap_or_else(now_secs))
}

/// 自动过期 TTL(秒):0 天 = 关闭 → None。从 CONFIG 实时读(设置热重载即生效)。
/// The auto-expiry TTL in seconds: 0 days = off -> None. Read live from CONFIG (a hot
/// reload takes effect immediately).
pub(super) fn ttl_secs() -> Option<u64> {
    let days = CONFIG
        .read()
        .map(|c| c.clipboard.auto_expire_days)
        .unwrap_or(0);
    if days == 0 {
        None
    } else {
        Some(days as u64 * 86400)
    }
}

/// 清理过期条目(纯函数,同步):非置顶且 copied_at 存在且 `now - copied_at >= ttl`
/// → 删除;置顶永不过期;无时间戳(旧条目)不过期。图片缓存按引用规则同步清理
/// (与 delete/truncate 一致:hash 仍被幸存条目引用则保留)。ttl_secs = None 表示
/// 关闭,直接返回 0。返回删除条数。
/// Expire entries (pure, synchronous): unpinned entries with a timestamp whose
/// `now - copied_at >= ttl` are removed; pinned entries never expire; legacy entries
/// without a timestamp never expire. Image cache files follow the reference rules
/// (same as delete/truncate: a hash still referenced by a survivor is kept).
/// ttl_secs = None disables expiry (returns 0). Returns the number removed.
pub(super) fn expire_entries(
    history: &mut Vec<ClipEntry>,
    now_secs: u64,
    ttl_secs: Option<u64>,
) -> usize {
    let Some(ttl) = ttl_secs else {
        return 0;
    };
    let mut dropped: Vec<ClipEntry> = Vec::new();
    history.retain(|e| {
        // 时间回拨(now < copied_at):saturating_sub 为 0,未到 ttl,天然安全。
        // Clock rollback (now < copied_at): saturating_sub yields 0, under ttl, safe.
        let expired = !e.pinned
            && e.copied_at
                .map(|t| now_secs.saturating_sub(t) >= ttl)
                .unwrap_or(false);
        if expired {
            dropped.push(e.clone());
        }
        !expired
    });
    for d in &dropped {
        cache_delete_for_removed(history, d);
    }
    dropped.len()
}

// 图片去重哈希已在 crate::hash 统一实现;下方注释块为第二阶段(内容哈希)预案。
// The dedup hash itself now lives in crate::hash; the block below is the phase-2
// (content hash) plan.

/*
【第二阶段】图片内容哈希:解码 PNG → 画成 16x16 缩略图 → 对 TIFF 字节做 FNV-1a。
同一张图即使被应用重新编码(字节不同)也得到相同哈希;解码失败回退原始字节哈希。
第一阶段暂不使用(去重只按原始字节哈希,同图不同编码的去重留待后续启用)。
启用时:去掉本块注释,并在 record_image 里恢复 image_content_hash 调用与
ClipEntry.image_hash 字段(结构体、record_text、record_image、测试 helper 同步补回)。

[PHASE 2] Image CONTENT hash: decode the PNG -> draw a 16x16 thumbnail -> FNV-1a over
its TIFF bytes. Re-encoded copies of the same image (different bytes) hash identically;
decoding failures fall back to the raw-byte hash. Deferred: phase 1 dedups by the raw
byte hash only (cross-encoding dedup waits for this). To enable: uncomment this block and
restore the image_content_hash call + the ClipEntry.image_hash field in record_image
(also in the struct, record_text, and the test helpers).
pub(super) unsafe fn image_content_hash(png: &[u8]) -> u64 {
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: png.as_ptr() as *const c_void,
        length: png.len()
    ];
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithData: data];
    if img.is_null() {
        return fnv1a64(png);
    }
    let thumb: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let thumb: *mut AnyObject = msg_send![thumb, initWithSize: NSSize::new(16.0, 16.0)];
    let _: () = msg_send![thumb, lockFocus];
    let dst = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(16.0, 16.0));
    let src = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let op: usize = 1; // NSCompositingOperationCopy
    let _: () = msg_send![img, drawInRect: dst, fromRect: src, operation: op, fraction: 1.0f64];
    let _: () = msg_send![thumb, unlockFocus];
    let tiff: *mut AnyObject = msg_send![thumb, TIFFRepresentation];
    if tiff.is_null() {
        return fnv1a64(png);
    }
    let len: usize = msg_send![tiff, length];
    let ptr: *const c_void = msg_send![tiff, bytes];
    if ptr.is_null() || len == 0 {
        return fnv1a64(png);
    }
    fnv1a64(std::slice::from_raw_parts(ptr as *const u8, len))
}
*/

/// 新条目(非置顶)应插入的位置:置顶区之后(第一个非置顶条目的下标)。
/// The insertion index for a new (unpinned) entry: right after the pinned block.
pub(super) fn insert_position(history: &[ClipEntry]) -> usize {
    history.iter().take_while(|e| e.pinned).count()
}

/// 按文本查找条目下标;找不到返回 None。
/// Find an entry's index by text; None when absent.
pub(super) fn find_by_text(history: &[ClipEntry], text: &str) -> Option<usize> {
    history.iter().position(|e| e.text == text)
}

/// 把已存在的条目提到最前:保留其置顶状态——置顶条目移到置顶区顶部,
/// 未置顶条目移到非置顶区顶部(即"最新位置")。列表因此永不重复。
/// Move an existing entry to the front, KEEPING its pinned state: pinned entries go to the
/// top of the pinned block, unpinned ones to the top of the unpinned block (the newest
/// slot). The list therefore never holds duplicates.
pub(super) fn move_entry_to_front(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx >= history.len() {
        return;
    }
    let e = history.remove(idx);
    let pos = if e.pinned {
        0
    } else {
        insert_position(history)
    };
    history.insert(pos, e);
}

/// 把新文本记入历史。规则:
/// - 空文本忽略
/// - 全表查重:文本已存在 → 把旧条目提到最前(保留置顶状态,见 move_entry_to_front),
///   并把来源(名称 + 图标键)更新为本次复制的来源(它是"最新一次复制"的来源)
/// - 未命中 → 新条目插到置顶区之后;超出 max 裁剪最旧条目
///
/// 返回是否真正写入。
///
/// Record a new text into the history:
/// - empty text is ignored
/// - full-list dedup: an existing text is moved to the front (pinned state kept, see
///   move_entry_to_front), and its source (name + icon key) is updated to this copy's source
///   (it is the "latest copy" now)
/// - a new text is inserted after the pinned block; entries beyond `max` are trimmed
///
/// Returns whether something was actually recorded.
pub(super) fn record_text(
    history: &mut Vec<ClipEntry>,
    text: &str,
    source: &str,
    source_key: &str,
    max: usize,
) -> bool {
    if text.is_empty() || max == 0 {
        return false;
    }
    if let Some(idx) = find_by_text(history, text) {
        history[idx].source_app = source.to_string();
        history[idx].source_key = source_key.to_string();
        // 去重移前 = 最近一次复制:刷新时间戳,过期从“最后一次复制”重新计时。
        // A dedup-move = the latest copy: refresh the timestamp so expiry counts from
        // the most recent copy, not the first one.
        history[idx].copied_at = Some(now_secs());
        move_entry_to_front(history, idx);
        return true;
    }
    let pos = insert_position(history);
    history.insert(
        pos,
        ClipEntry {
            text: text.to_string(),
            image: None,
            pinned: false,
            source_app: source.to_string(),
            source_key: source_key.to_string(),
            copied_at: Some(now_secs()),
        },
    );
    if history.len() > max {
        // 超上限裁剪时,被裁图片条目的缓存文件一并删除——但仅当其 hash 不再被
        // 幸存条目引用(同 hash 的文件/数据条目可能共存,共享缓存)。
        // When trimming beyond the cap, drop the trimmed image entries' cache files too --
        // but only when the hash is no longer referenced by a survivor (same-hash
        // file/data entries may coexist and share the cache).
        for dropped in &history[max..] {
            cache_delete_for_removed(&history[..max], dropped);
        }
        history.truncate(max);
    }
    true
}

/// 把一张图片记入历史(两种条目,**各自按类去重,不跨类**):
/// - **数据条目**(image-data copies,字节已由调用方落盘):查重按内容哈希
/// - **文件复制条目**(file copies,只读一次内容算哈希 + 缩略图,字节不落盘):
///   查重按内容哈希——原文件与它在访达里的副本(不同路径、同样字节)只保留一条;
///   解码失败的退化条目(hash=0)按来源路径查重
///
/// 规则与 record_text 一致:
/// - 空预览且无来源路径(录制失败)忽略
/// - 已存在(同哈希 / 同路径)→ 旧条目提到最前(保留置顶),来源更新为本次复制来源;
///   文件条目的来源路径一并更新为最新一次复制
/// - 未命中 → 新条目插到置顶区之后;超出 max 裁剪
///   同图不同编码的去重留待第二阶段(见被注释的 image_content_hash)。
///
/// Record an image into the history (two kinds, EACH deduped within its own class):
/// - a DATA entry (image-data copy, bytes already cached by the caller): dedup by content
///   hash
/// - a FILE-COPY entry (the file is read once for a hash + thumbnail, bytes never
///   stored): dedup by content hash -- a file and its Finder duplicate (different paths,
///   identical bytes) collapse into one entry; degenerate entries whose decode failed
///   (hash=0) fall back to dedup by source path
///
/// Same rules as record_text:
/// - an empty preview AND no source path (recording failed) is ignored
/// - an existing entry (same hash / same path) moves to the front (pinned kept), the
///   source updates; a file entry's source path also updates to the latest copy
/// - a new entry is inserted after the pinned block; entries beyond `max` are trimmed
///   (cross-encoding dedup waits for phase 2, see the commented-out image_content_hash).
pub(super) fn record_image(
    history: &mut Vec<ClipEntry>,
    image: &ImageEntry,
    source: &str,
    source_key: &str,
    max: usize,
) -> bool {
    if (image.preview_png.is_empty() && image.source_path.is_none()) || max == 0 {
        return false;
    }
    // 文件条目按内容哈希在文件条目内查重;退化条目(hash=0)按来源路径查重;
    // 数据条目按内容哈希在数据条目内查重。三类互不跨类。
    // File entries dedup by content hash among file entries (degenerate hash=0 entries by
    // path); data entries dedup by content hash among data entries. Never across classes.
    let dedup_hit = if image.source_path.is_some() {
        history.iter().position(|e| {
            e.image.as_ref().is_some_and(|i| {
                i.source_path.is_some()
                    && if image.hash != 0 {
                        i.hash == image.hash
                    } else {
                        i.source_path.as_deref() == image.source_path.as_deref()
                    }
            })
        })
    } else {
        history.iter().position(|e| {
            e.image
                .as_ref()
                .is_some_and(|i| i.source_path.is_none() && i.hash == image.hash)
        })
    };
    if let Some(idx) = dedup_hit {
        history[idx].source_app = source.to_string();
        history[idx].source_key = source_key.to_string();
        // 去重移前 = 最近一次复制:刷新时间戳(同 record_text)。
        // A dedup-move = the latest copy: refresh the timestamp (same as record_text).
        history[idx].copied_at = Some(now_secs());
        // 文件条目去重命中:来源路径更新为最新一次复制(粘贴恢复最新文件)。
        // A file-entry dedup hit: the source path updates to the latest copy (pasting
        // restores the newest file).
        if image.source_path.is_some() {
            history[idx].image.as_mut().unwrap().source_path = image.source_path.clone();
        }
        move_entry_to_front(history, idx);
        return true;
    }
    let pos = insert_position(history);
    history.insert(
        pos,
        ClipEntry {
            // 文件引用条目把文件名放进 text:行内显示 + 可被搜索(粘贴走 image 分支,
            // text 不会参与粘贴)。数据条目保持空 text。
            // File-reference entries keep the filename in text: the row shows it and it is
            // searchable (paste goes through the image branch; text never gets pasted).
            // Data entries keep an empty text.
            text: image
                .source_path
                .as_deref()
                .map(|p| p.rsplit('/').next().unwrap_or("").to_string())
                .unwrap_or_default(),
            image: Some(image.clone()),
            pinned: false,
            source_app: source.to_string(),
            source_key: source_key.to_string(),
            copied_at: Some(now_secs()),
        },
    );
    if history.len() > max {
        // 超上限裁剪时,被裁图片条目的缓存文件一并删除——但仅当其 hash 不再被
        // 幸存条目引用(同 hash 的文件/数据条目可能共存,共享缓存)。
        // When trimming beyond the cap, drop the trimmed image entries' cache files too --
        // but only when the hash is no longer referenced by a survivor (same-hash
        // file/data entries may coexist and share the cache).
        for dropped in &history[max..] {
            cache_delete_for_removed(&history[..max], dropped);
        }
        history.truncate(max);
    }
    true
}

/// 置顶第 idx 条:移到置顶区顶部(最上)。返回条目的**新历史索引**(恒为 0)。
/// Pin entry `idx`: move it to the top of the pinned block. Returns the entry's NEW
/// history index (always 0).
pub(super) fn pin_entry(history: &mut Vec<ClipEntry>, idx: usize) -> usize {
    if idx >= history.len() || history[idx].pinned {
        return idx;
    }
    let mut e = history.remove(idx);
    e.pinned = true;
    history.insert(0, e);
    0
}

/// 取消第 idx 条的置顶:移到非置顶区顶部(最新位置)。返回条目的**新历史索引**(= 插
/// 入位置,紧随最后一个置顶条目)。
/// Unpin entry `idx`: move it to the top of the unpinned block (the newest slot). Returns
/// the entry's NEW history index (the insert position, right after the last pinned entry).
pub(super) fn unpin_entry(history: &mut Vec<ClipEntry>, idx: usize) -> usize {
    if idx >= history.len() || !history[idx].pinned {
        return idx;
    }
    let mut e = history.remove(idx);
    e.pinned = false;
    let pos = insert_position(history);
    history.insert(pos, e);
    pos
}

/// 切换第 idx 条的置顶状态;返回 (切换后的状态, 条目的新历史索引)。纯函数,图钉
/// 按钮回调与 ← 键盘快捷键共用,单测覆盖。新索引供"跟随置顶"选中定位——旧索引在
/// 重排后会指向别的条目,不能用。
/// Toggle the pinned state of entry `idx`; returns (the new state, the entry's NEW history
/// index). Pure function shared by the pin-button callback and the ← shortcut; unit-tested.
/// The new index feeds "follow-pin" selection -- the OLD index points at a different entry
/// once the list is reordered, so it must not be used.
pub(super) fn toggle_pin_on(history: &mut Vec<ClipEntry>, idx: usize) -> (bool, usize) {
    let Some(entry) = history.get(idx) else {
        return (false, idx);
    };
    let pinned = entry.pinned;
    let new_idx = if pinned {
        unpin_entry(history, idx)
    } else {
        pin_entry(history, idx)
    };
    (!pinned, new_idx)
}

/// 删除第 idx 条(越界忽略),图片条目的缓存文件一并删除。
/// Delete entry `idx` (out of range is ignored); an image entry's cache file goes too.
pub(super) fn delete_entry(history: &mut Vec<ClipEntry>, idx: usize) {
    if idx < history.len() {
        let removed = history.remove(idx);
        cache_delete_for_removed(history, &removed);
    }
}

/// 条目筛选:全部 / 文本 / 图片 / 链接 / 代码片段。
/// Picker filters: All / Text / Image / Link / Code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClipFilter {
    All,
    Text,
    Image,
    Link,
    Code,
}

/// 当前生效的筛选项 / the active filter.
pub(super) static CLIP_FILTER: Mutex<ClipFilter> = Mutex::new(ClipFilter::All);

/// Tab 键的分类循环顺序:全部 → 文本 → 图片 → 链接 → 代码 → 全部。
/// The Tab filter cycle: All -> Text -> Image -> Link -> Code -> All.
pub(super) fn next_clip_filter(filter: ClipFilter) -> ClipFilter {
    match filter {
        ClipFilter::All => ClipFilter::Text,
        ClipFilter::Text => ClipFilter::Image,
        ClipFilter::Image => ClipFilter::Link,
        ClipFilter::Link => ClipFilter::Code,
        ClipFilter::Code => ClipFilter::All,
    }
}

/// 条目是否命中筛选项 / whether an entry matches the filter.
pub(super) fn matches_filter(e: &ClipEntry, f: ClipFilter) -> bool {
    match f {
        ClipFilter::All => true,
        ClipFilter::Image => e.image.is_some(),
        ClipFilter::Text => e.image.is_none() && classify_text(&e.text) == TextKind::Plain,
        ClipFilter::Link => e.image.is_none() && classify_text(&e.text) == TextKind::Url,
        ClipFilter::Code => e.image.is_none() && classify_text(&e.text) == TextKind::Code,
    }
}

/// 过滤:返回匹配 query + 筛选项的历史索引列表(空 query + All = 全部;大小写不敏感
/// 子串匹配)。/ Filter: indices of entries matching the query AND the filter (empty query
/// + All = every entry; case-insensitive substring match).
pub(super) fn filtered_indices(
    history: &[ClipEntry],
    query: &str,
    filter: ClipFilter,
) -> Vec<usize> {
    let q = query.to_lowercase();
    history
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches_filter(e, filter) && (q.is_empty() || e.text.to_lowercase().contains(&q))
        })
        .map(|(i, _)| i)
        .collect()
}

/// 显示索引 → 历史索引(经当前过滤列表映射;越界返回 None)。
/// Display index -> history index (via the current filtered list; None when out of range).
pub(super) fn mapped_index(display_idx: usize) -> Option<usize> {
    FILTERED.lock().unwrap().get(display_idx).copied()
}

/// 当前生效的最大条数(从 CONFIG 读,设置保存后下次轮询生效)。
/// The effective max entry count (read from CONFIG; takes effect on the next poll).
pub(super) fn max_entries() -> usize {
    CONFIG
        .read()
        .map(|c| c.clipboard.max_entries as usize)
        .unwrap_or(50)
        .clamp(1, 100)
}
