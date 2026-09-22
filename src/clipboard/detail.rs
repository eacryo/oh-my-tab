//! 剪贴板子系统 · detail:条
//! 目详情滚动视图

use super::*;

// ========== 详情滚动视图 ==========

pub(super) unsafe fn add_detail_text(
    content: *mut AnyObject,
    text: &str,
    w: f64,
    h: f64,
    kind: TextKind,
    prepared_code: Option<&PreparedCodeDisplay>,
    code_soft_wrap: bool,
) {
    let is_code = kind == TextKind::Code;
    debug_assert!(!code_soft_wrap || (is_code && prepared_code.is_some()));
    // 滚动视图向右延伸到面板边缘,文本视图内部保留右侧内边距给原生 overlay scroller。
    // Extend the scroll view to the panel edge and keep right padding inside the text view for
    // the native overlay scroller.
    let scroll_w = w - DETAIL_PAD;
    let body_h = (h - DETAIL_CHROME_H).max(DETAIL_LINE_H);
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![
        scroll,
        initWithFrame: NSRect::new(
            NSPoint::new(DETAIL_PAD, DETAIL_TOOLBAR_H),
            NSSize::new(scroll_w, body_h)
        )
    ];
    let _: () = msg_send![scroll, setBorderType: 0u64]; // NSNoBorder
    let clear_background: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![scroll, setBackgroundColor: clear_background];
    let _: () = msg_send![scroll, setDrawsBackground: false];
    // 使用 AppKit Overlay 滚动条,不为横纵滚动条预留交叉 corner;滚动端点、拖拽和滚轮
    // 相位全部交给 NSScrollView。NSClipView 也关闭背景绘制,避免默认白块。
    // Use AppKit Overlay scrollers, which reserve no horizontal/vertical corner. NSScrollView
    // owns endpoints, dragging, and wheel phases; NSClipView also stops drawing its background
    // to avoid the default white corner.
    let no_wrap = is_code && !code_soft_wrap;
    // 关闭系统滚动条,避免它在滚动/布局回调中重新出现并与自定义胶囊重叠。
    // Disable native scrollers so they cannot reappear during scrolling/layout and overlap the
    // always-visible custom capsules.
    let _: () = msg_send![scroll, setHasVerticalScroller: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![scroll, setAutohidesScrollers: true];
    let _: () = msg_send![scroll, setScrollerStyle: 1isize]; // NSScrollerStyleOverlay
    let clip_view: *mut AnyObject = msg_send![scroll, contentView];
    if !clip_view.is_null() {
        let _: () = msg_send![clip_view, setDrawsBackground: false];
    }
    // 端点橡皮筋完全交给原生 elasticity(Automatic:内容溢出的方向才回弹)。
    // 千万不要在 bounds 通知里硬钳越界原点来"关闭"它——手势进行中改写 clipView
    // 会污染 NSScrollView 的动量累加基准,后续惯性事件从脏基准重新施加 delta,
    // 与硬钳反复拉锯,滚动条就会在端点抽搐(见 detail_scroll_indicator_bounds_changed)。
    // Endpoint rubber banding is left entirely to native elasticity (Automatic: bounces
    // only on axes whose content overflows). Never "disable" it by hard-clamping
    // out-of-range origins inside the bounds notification -- rewriting the clip view
    // mid-gesture poisons NSScrollView's momentum base, so following momentum events
    // reapply their deltas from the stale base and fight the clamp, making the scrollbar
    // twitch at the endpoints (see detail_scroll_indicator_bounds_changed).
    let _: () = msg_send![scroll, setVerticalScrollElasticity: 0isize]; // NSScrollElasticityAutomatic
    let _: () = msg_send![scroll, setHorizontalScrollElasticity: 0isize];

    // 软换行关闭时保留原文宽度并启用横向滚动;开启时继续使用 U+2028 显示模型,不显示
    // 横向滚动条。
    // With soft wrap off, preserve raw line width and enable horizontal scrolling. When on,
    // use the U+2028 display model and hide the horizontal scroller.
    // 详情打开路径已准备软换行模型;这里只借用文本和映射,不复制长文本。
    // The open path has already prepared the soft-wrap model; borrow its text and mapping here
    // without copying the long source.
    let prepared = prepared_code;
    let display_text = prepared.map(|code| code.text.as_str()).unwrap_or(text);
    // 自定义软换行只在显示文本中插入行分隔符;共享缓存里的原文映射,复制选区时
    // 去掉这些显示字符,不复制长原文/边界数组。
    // Custom soft wrapping inserts line separators only into display text. Share the cached
    // source map so copied selections omit those display characters without cloning the long
    // source or boundary array.
    *DETAIL_SOURCE_MAP.lock().unwrap() = prepared.and_then(|code| code.source_map.clone());
    let tv: *mut AnyObject = msg_send![detail_text_view_class(), alloc];
    // 宽度必须等于安装后的真实宽度(scroll_w = clip 宽)。用更窄的 avail_w 构建,
    // NSClipView 装 doc 时会把它拉伸到 scroll_w,宽度差迫使 TextKit 显示时整体重排,
    // 并触发把视口拖离顶部的滚动动画(→ 打开后滚动条不在最顶的根因)。
    // The width must equal the real post-install width (scroll_w = clip width). Building
    // narrower (avail_w) makes NSClipView stretch the document view on install; the width
    // delta forces a full TextKit re-layout at display time and spawns the scroll animation
    // that drags the viewport away from the top (the scrollbar-not-at-top-after-open bug).
    let tv: *mut AnyObject = msg_send![
        tv,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(scroll_w, body_h))
    ];
    *DETAIL_SOFT_WRAP_TEXT_VIEW.lock().unwrap() = code_soft_wrap.then_some(ObjPtr::new(tv));
    // 详情必须在显示前完成完整布局。非连续/后台布局会在面板出现后分批增大
    // documentView,NSClipView 为维持旧可见区域会同步改变 bounds.origin.y,这正是
    // 滚动条打开后向下跳的根因。
    // Detail must finish layout before display. Non-contiguous/background layout grows the
    // document view in batches after the panel appears, and NSClipView changes bounds.origin.y
    // to preserve the old visible region—the root cause of the post-open downward jump.
    let layout: *mut AnyObject = msg_send![tv, layoutManager];
    let _: () = msg_send![layout, setAllowsNonContiguousLayout: false];
    let _: () = msg_send![layout, setBackgroundLayoutEnabled: false];

    let ns_text = make_nsstring(display_text);
    let _: () = msg_send![tv, setString: ns_text];
    CFRelease(ns_text as *const c_void);

    // 代码只保留等宽排版和软换行,不再做语法着色;URL 继续沿用列表的蓝色。
    // Code retains only monospace layout and soft wrapping, with no syntax coloring; URLs keep
    // the list's blue color.
    let storage: *mut AnyObject = msg_send![tv, textStorage];
    // 链接颜色、字体和段落样式会修改 NSTextStorage;外层事务合并 TextKit
    // 无效化/修复,避免软换行详情在布局前重复处理全文。
    // Link color, font, and paragraph styles mutate NSTextStorage. An outer transaction coalesces
    // TextKit invalidation/fix-up instead of repeatedly processing the soft-wrapped detail.
    let _: () = msg_send![storage, beginEditing];
    apply_link_color(storage, display_text, kind);

    let _: () = msg_send![tv, setEditable: false];
    // 可选中(之前禁用了选中,长文本没法复制其中一部分)。非 key 窗口里 NSTextView
    // 仍支持鼠标拖选(选中显示灰色);复制交给原生路径——右键菜单,以及主浮窗
    // container_key_down 对 Cmd+C 的转发(见 copy_detail_selection)。
    // Selectable (it used to be disabled, so a part of a long text could never be
    // copied). In a non-key window NSTextView still supports mouse-drag selection
    // (shown gray); copying goes through the native paths -- the right-click menu, and
    // the Cmd+C forwarding in the picker's container_key_down (see copy_detail_selection).
    let _: () = msg_send![tv, setSelectable: true];
    let _: () = msg_send![tv, setDrawsBackground: false];
    // 普通详情文字使用 14pt;代码使用等宽 14pt,让列宽和断点计算稳定。
    // Plain detail text uses 14pt; code uses a 14pt monospaced font for stable columns/breaks.
    let font: *mut AnyObject = if is_code {
        msg_send![class!(NSFont), monospacedSystemFontOfSize: 14.0f64, weight: 0.0f64]
    } else {
        msg_send![class!(NSFont), systemFontOfSize: 14.0f64]
    };
    let _: () = msg_send![tv, setFont: font];
    if code_soft_wrap {
        apply_code_paragraph_styles(storage, display_text);
    }
    // 代码详情两种模式(软换行/非软换行)的段内空格都显示为中点,统一淡色染色,
    // 与列表行的空格标记观感一致。普通文本/链接无中点,调用无副作用。
    if is_code {
        apply_visible_space_markers(storage, display_text);
    }
    let _: () = msg_send![storage, endEditing];
    // 详情窗口外框对齐条目内容块顶部,正文再补上列表的行内顶部留白,避免整体窗口下移。
    // The detail frame aligns with the row content block; add the list's top padding inside
    // the text view so the whole detail window does not shift downward.
    let _: () = msg_send![tv, setTextContainerInset: NSSize::new(0.0, ROW_PAD_TOP)];
    let text_container: *mut AnyObject = msg_send![tv, textContainer];
    if is_code {
        // 去掉 NSTextView 默认的行内留白,让代码内容边界由 DETAIL_PAD 统一控制。
        // Remove NSTextView's default line padding so DETAIL_PAD controls the code boundary.
        let _: () = msg_send![text_container, setLineFragmentPadding: 0.0f64];
    }
    // 先按最终宽度完成全部 TextKit 布局,再把 documentView 高度固定为完整 usedRect。
    // 若让 NSTextView 在显示后继续 verticallyResizable,它仍会异步改 frame 并推走顶部。
    // Complete all TextKit layout at the final width, then freeze documentView height to the
    // full usedRect. Leaving NSTextView vertically resizable after display would still mutate
    // its frame asynchronously and push the viewport away from the top.
    let _: () = msg_send![text_container, setWidthTracksTextView: !no_wrap];
    // 与安装后的真实宽度保持一致(见上方 initWithFrame 的说明)。
    // Keep this consistent with the real post-install width (see the initWithFrame note above).
    let container_w = if no_wrap { 1_000_000_000.0 } else { scroll_w };
    let _: () = msg_send![
        text_container,
        setContainerSize: NSSize::new(container_w, 1_000_000_000.0)
    ];
    let _: () = msg_send![tv, setHorizontallyResizable: no_wrap];
    let _: () = msg_send![tv, setVerticallyResizable: true];
    let _: () = msg_send![layout, ensureLayoutForTextContainer: text_container];
    let used: NSRect = msg_send![layout, usedRectForTextContainer: text_container];
    let document_w = if no_wrap {
        (used.origin.x + used.size.width + DETAIL_PAD)
            .ceil()
            .max(scroll_w)
    } else {
        scroll_w
    };
    let document_h = (used.origin.y + used.size.height + ROW_PAD_TOP * 2.0)
        .ceil()
        .max(body_h);
    let _: () = msg_send![tv, setFrameSize: NSSize::new(document_w, document_h)];
    let _: () = msg_send![tv, setVerticallyResizable: false];
    let _: () = msg_send![tv, setSelectedRange: NSRange::new(0, 0)];
    let _: () = msg_send![scroll, setDocumentView: tv];
    release_obj(tv);
    *DETAIL_TEXT_VIEW.lock().unwrap() = Some(ObjPtr::new(tv));

    // 详情滚动条由自定义胶囊绘制;bounds 通知只负责刷新胶囊位置,端点橡皮筋由原生
    // elasticity 处理,这里不改写 bounds(改写会在手势中与动量拉锯,导致抽搐)。
    // Detail scrollbars are drawn by the custom capsules; the bounds notification only
    // refreshes capsule positions. Endpoint rubber banding is handled by native
    // elasticity -- never rewrite bounds here (doing so fights momentum mid-gesture and
    // causes the endpoint twitch).
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setPostsBoundsChangedNotifications: true];
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let bounds_name = make_nsstring("NSViewBoundsDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(detailScrollIndicatorBoundsChanged:),
        name: bounds_name,
        object: clip
    ];
    CFRelease(bounds_name as *const c_void);
    *DETAIL_SCROLL_VIEW.lock().unwrap() = Some(ObjPtr::new(scroll));

    // 系统滚动条已完全关闭;实际滚动仍由 NSScrollView/NSClipView 处理,自定义视图只负责视觉
    // 与拖拽映射,因此不会出现两层滚动条。
    // Native scrollers are fully disabled; NSScrollView/NSClipView still perform scrolling,
    // while the custom views handle visuals and thumb dragging, preventing double scrollbars.
    let vertical_indicator: *mut AnyObject = msg_send![scroll_indicator_class(), alloc];
    let vertical_indicator: *mut AnyObject = msg_send![
        vertical_indicator,
        initWithFrame: NSRect::new(
            NSPoint::new(
                scroll_w - SCROLL_INDICATOR_HIT_W - SCROLL_INDICATOR_EDGE,
                SCROLL_INDICATOR_EDGE,
            ),
            NSSize::new(
                SCROLL_INDICATOR_HIT_W,
                body_h - (SCROLL_INDICATOR_EDGE * 2.0) - SCROLL_INDICATOR_CORNER_RESERVE,
            )
        )
    ];
    update_scroll_indicator_visual(
        vertical_indicator,
        body_h - (SCROLL_INDICATOR_EDGE * 2.0) - SCROLL_INDICATOR_CORNER_RESERVE,
        false,
    );
    let _: () = msg_send![scroll, addSubview: vertical_indicator];
    *DETAIL_SCROLL_INDICATOR.lock().unwrap() = Some(ObjPtr::new(vertical_indicator));
    release_obj(vertical_indicator);

    if no_wrap {
        let horizontal_indicator: *mut AnyObject = msg_send![scroll_indicator_class(), alloc];
        let horizontal_indicator: *mut AnyObject = msg_send![
            horizontal_indicator,
            initWithFrame: NSRect::new(
                NSPoint::new(
                    3.0,
                    body_h - SCROLL_INDICATOR_HIT_W - 3.0,
                ),
                NSSize::new(
                    scroll_w
                        - (SCROLL_INDICATOR_EDGE * 2.0)
                        - SCROLL_INDICATOR_CORNER_RESERVE,
                    SCROLL_INDICATOR_HIT_W,
                )
            )
        ];
        update_scroll_indicator_visual(
            horizontal_indicator,
            scroll_w - (SCROLL_INDICATOR_EDGE * 2.0) - SCROLL_INDICATOR_CORNER_RESERVE,
            true,
        );
        let _: () = msg_send![scroll, addSubview: horizontal_indicator];
        *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() =
            Some(ObjPtr::new(horizontal_indicator));
        release_obj(horizontal_indicator);
    } else {
        *DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap() = None;
    }
    update_scroll_indicator_for(ScrollTarget::Detail);
    if no_wrap {
        update_scroll_indicator_for(ScrollTarget::DetailHorizontal);
    }
    let _: () = msg_send![content, addSubview: scroll];
    release_obj(scroll);

    // 详情文本上显示 I-beam 输入光标:非 key 窗口里 cursor rect 不生效(NSTextView
    // 自带的 I-beam 矩形只在 key 窗口激活,详情面板不会成为 key → 之前一直箭头)。
    // 用与行悬停同款的 mouseEntered/Exited + ActiveAlways tracking area 手动设置
    // NSCursor;cursorUpdate 选项明确不支持 ActiveAlways(见 NSTrackingArea.h),
    // 所以走 enter/exit 路径。tracking area 放在固定大小的滚动视图上——每次打开
    // 详情都是新视图,rect 不会随文本增长而过期。
    // Show the I-beam over the detail text: cursor rects apply only to the KEY window
    // (NSTextView's own I-beam rect never activates in the non-key panel -> it used to be
    // an arrow). A mouseEntered/Exited + ActiveAlways tracking area (identical to the row
    // hover) sets NSCursor manually; cursorUpdate is documented as NOT supported with
    // ActiveAlways (NSTrackingArea.h), so the enter/exit path is used. The area sits on
    // the fixed-size scroll view -- a fresh view per detail open, so the rect never goes
    // stale as the text grows.
    let opts: u64 = 0x01 | 0x80; // MouseEnteredAndExited | ActiveAlways
    let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let bounds: NSRect = msg_send![scroll, bounds];
    let ta: *mut AnyObject = msg_send![
        ta,
        initWithRect: bounds,
        options: opts,
        owner: observer(),
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![scroll, addTrackingArea: ta];
    release_obj(ta);
}

/// 详情文本光标:进入 → I-beam(输入光标)。非 key 窗口里 cursor rect 只在 key 窗口
/// 生效,NSTextView 自带的 I-beam 矩形从不激活,鼠标在详情文本上一直显示箭头;
/// 这里用 ActiveAlways 的 mouseEntered/Exited tracking area(owner = observer,与行
/// 悬停同款)手动设置 NSCursor。cursorUpdate 选项不支持 ActiveAlways(NSTrackingArea.h
/// 明确标注),所以不能走 cursorUpdate 路径。
/// The detail-text cursor: entering -> I-beam. Cursor rects apply only to the key window,
/// so NSTextView's own I-beam rect never activates in the non-key panel and the mouse
/// showed an arrow over the text; an ActiveAlways mouseEntered/Exited tracking area
/// (owner = the observer, same as the row hover) sets NSCursor manually. cursorUpdate is
/// documented as unsupported with ActiveAlways (NSTrackingArea.h), hence the enter/exit
/// path.
pub(super) extern "C" fn detail_tv_cursor_entered(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let ibeam: *mut AnyObject = msg_send![class!(NSCursor), IBeamCursor];
        let _: () = msg_send![ibeam, set];
    }
}

/// 详情文本光标:离开 → 恢复默认箭头。
/// The detail-text cursor: leaving -> back to the default arrow.
pub(super) extern "C" fn detail_tv_cursor_exited(
    _self: *mut c_void,
    _cmd: Sel,
    _event: *mut c_void,
) {
    unsafe {
        let arrow: *mut AnyObject = msg_send![class!(NSCursor), arrowCursor];
        let _: () = msg_send![arrow, set];
    }
}

/// 在当前筛选结果中找回指定文本条目的显示下标。历史全局按文本去重,因此文本可作为
/// 详情条目的稳定身份;复制出的片段插入历史后,原详情仍应保持选中并继续显示。
/// Find a text entry's display index in the current filters. History deduplicates text
/// globally, so text is a stable detail-entry identity; after a copied excerpt is inserted,
/// the source detail must stay selected and remain visible.
/// 空态文档至少覆盖最小列表区,其余情况下恰好等于实时可视区高度,供提示真正居中。
/// The empty-state document covers at least the minimum list area, otherwise exactly the
/// live visible height so its hint is truly centered.
pub(super) fn empty_state_doc_height(visible_h: f64) -> f64 {
    visible_h.max(picker_min_height() - header_strip_h() - FOOTER_H)
}

pub(super) fn visible_selection_for_text(
    history: &[ClipEntry],
    query: &str,
    filter: ClipFilter,
    text: &str,
) -> Option<usize> {
    let history_idx = history
        .iter()
        .position(|entry| entry.image.is_none() && entry.text == text)?;
    filtered_indices(history, query, filter)
        .iter()
        .position(|&idx| idx == history_idx)
}

/// 悬停样式门禁(纯函数,单测覆盖):指针不在浮窗窗口内时,悬停索引一律按无效
/// 处理(NO_SELECTION)。HOVER_ROW 只反映最后一次 enter/exited 事件,事件丢失
/// (REBUILDING 抑制、跨面板穿越、行视图拆除)会让它冻结成残留值——重建时套到
/// 新行上就是幽灵悬停底。
/// The hover-style gate (pure; unit-tested): when the pointer is outside the picker
/// window, the hover index is always treated as invalid (NO_SELECTION). HOVER_ROW only
/// mirrors the last enter/exited event -- lost events (REBUILDING suppression, cross-panel
/// transitions, row teardown) can freeze it into a stale value that would paint a phantom
/// hover fill onto rebuilt rows.
pub(super) fn effective_hover_row(pointer_in_window: bool, hover_row: usize) -> usize {
    if pointer_in_window {
        hover_row
    } else {
        NO_SELECTION
    }
}

pub(super) fn rect_contains_point(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y <= rect.origin.y + rect.size.height
}

/// 指针当前是否位于浮窗窗口内。NSEvent.mouseLocation 与窗口 frame 同为全局屏坐标
/// (底部原点),可直接包含判定。
/// Whether the pointer is currently inside the picker window. NSEvent.mouseLocation and
/// the window frame share global screen coordinates (bottom-left origin), so containment
/// is a direct comparison.
unsafe fn pointer_in_picker_window() -> bool {
    let Some(w) = *PICKER_WINDOW.lock().unwrap() else {
        return false;
    };
    let frame: NSRect = msg_send![w.0, frame];
    let mouse: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    rect_contains_point(frame, mouse)
}

/// 详情内复制后立即重建已打开的历史列表。不能等下一次呼出:轮询虽会写入内存,
/// 但已建好的行视图不会自行读取新历史。重建前恢复源详情的选择,避免新片段插到顶部后
/// 高亮改指向新条目而右侧仍显示旧详情。
/// Immediately rebuild the open history list after copying from detail. Polling writes to
/// memory, but existing row views do not read the new history until the next summon. Restore
/// the source-detail selection before rebuilding, so a new excerpt at the top does not make
/// the highlight point at it while the right panel still shows the old detail.
fn refresh_open_picker_after_detail_copy(source_detail_text: Option<&str>) {
    if !PICKER_VISIBLE.load(Ordering::SeqCst) {
        return;
    }
    // 本流程指针定义上在详情面板上,列表悬停不可能成立;显式清掉 HOVER_ROW,
    // 防止重建把残留值套到新插入的顶部条目上(幽灵悬停底)。第二道保险——
    // rebuild_rows 的指针门禁是主防线。
    // The pointer is by definition over the detail panel in this flow, so no list hover
    // can exist; clear HOVER_ROW explicitly so the rebuild never paints a stale value onto
    // the newly inserted top entry (the phantom hover fill). A second line of defense --
    // the pointer gate in rebuild_rows is the primary one.
    *HOVER_ROW.lock().unwrap() = NO_SELECTION;
    if let Some(text) = source_detail_text {
        let selection = {
            let history = CLIP_HISTORY.lock().unwrap();
            let query = with_clipboard_ui(|ui| ui.search_query.clone());
            let filter = *CLIP_FILTER.lock().unwrap();
            visible_selection_for_text(&history, &query, filter, text)
        };
        if let Some(selection) = selection {
            set_picker_selection(selection);
        }
    }
    unsafe { rebuild_rows() };
    // 新片段插入会让来源行下移;行重建完成、REBUILDING 已解除后只重算详情位置,
    // 不重建详情文本视图,从而保留用户当前的选中文本。
    // Inserting the excerpt moves the source row down. Once rebuilding releases REBUILDING,
    // recompute only the detail position without recreating its text view, preserving the
    // user's current text selection.
    reposition_detail();
}

/// 把详情文本视图的**选中范围**写入剪贴板(无选中则兜底复制全文),Toast 提示,
/// 详情面板保持打开(可能还要继续复制其他片段)。**不打 paste marker**——这是一次
/// 真实复制,应当正常进入历史(与粘贴回写的抑制语义相反)。
/// Copy the detail text view's SELECTION to the pasteboard (full text when nothing is
/// selected), toast, and keep the detail open (the user may copy more ranges). Does NOT
/// stamp the paste marker -- this is a genuine copy that should enter the history (the
/// opposite of the paste-write-back suppression).
pub(super) fn copy_detail_selection() {
    let tv = match *DETAIL_TEXT_VIEW.lock().unwrap() {
        Some(t) => t.0,
        None => return,
    };
    let source_detail_text = {
        let sel = picker_selection();
        mapped_index(sel).and_then(|history_idx| {
            CLIP_HISTORY
                .lock()
                .unwrap()
                .get(history_idx)
                .filter(|entry| entry.image.is_none())
                .map(|entry| entry.text.clone())
        })
    };
    unsafe {
        let sel_range: NSRange = msg_send![tv, selectedRange];
        let mapped = {
            let map = DETAIL_SOURCE_MAP.lock().unwrap();
            map.as_ref().map(|source_map| {
                (
                    source_map.source.clone(),
                    source_map.source_range(sel_range),
                )
            })
        };
        let text = if let Some((source, source_range)) = mapped {
            // 代码详情可能插入了显示换行;按映射从原文提取,避免把格式化字符复制出去。
            // Code details may contain display-only breaks; extract from the source mapping
            // so formatting characters are never copied.
            let source_ns = make_nsstring(&source);
            let sub: *mut AnyObject = msg_send![source_ns, substringWithRange: source_range];
            let text = nsstring_to_rust(sub);
            CFRelease(source_ns as *const c_void);
            text
        } else {
            let full: *mut AnyObject = msg_send![tv, string];
            if sel_range.length > 0 {
                let sub: *mut AnyObject = msg_send![full, substringWithRange: sel_range];
                nsstring_to_rust(sub)
            } else {
                nsstring_to_rust(full)
            }
        };
        write_pasteboard_text(&text, false);
        // 复制是由本应用主动发起的,立刻读回并重建;不能只依赖 0.5s 轮询或通知,
        // 否则详情保持打开时新片段会延迟到下次呼出才出现。
        // This copy originates in our app, so read it back and rebuild immediately instead
        // of relying only on the 0.5s poll/notification; otherwise the new excerpt appears
        // only after the next picker summon while detail remains open.
        poll_clipboard();
        refresh_open_picker_after_detail_copy(source_detail_text.as_deref());
        show_toast(&t("clipboard.toast_copied"));
    }
}

/// 构建浮窗窗口(一次)。/ Build the picker window (once).
///
/// 设置页实时预览时只更新玻璃视图和详情补偿层,不重建剪贴板内容。
/// During the settings live preview, update only the glass views and detail compensation layer;
/// do not rebuild clipboard content.
pub(crate) unsafe fn apply_glass_properties() {
    let style = match crate::config::effective_glass_style().as_str() {
        "clear" => 1i64,
        _ => 0i64,
    };
    let tint_hex = crate::config::parse_hex8(&crate::config::effective_glass_tint());
    let tint = crate::ffi::hex_to_ns_color(tint_hex);
    if let Some(glass) = *PICKER_GLASS.lock().unwrap() {
        let _: () = msg_send![glass.0, setStyle: style];
        let _: () = msg_send![glass.0, setTintColor: tint];
    }
    if let Some(glass) = *DETAIL_GLASS.lock().unwrap() {
        let _: () = msg_send![glass.0, setStyle: style];
        let _: () = msg_send![glass.0, setTintColor: tint];
    }
    if let Some(fill_layer) = *DETAIL_GLASS_FILL_LAYER.lock().unwrap() {
        let compensation_hex = (tint_hex & 0xFFFF_FF00) | DETAIL_INACTIVE_GLASS_COMPENSATION_A;
        crate::ffi::layer_set_background(
            fill_layer.0,
            crate::ffi::hex_to_cg_color(compensation_hex),
        );
    }
}

/// Apply the active light/dark appearance to already-created clipboard panels.
/// 将当前浅色/深色外观应用到已经创建的剪贴板面板。
pub(crate) unsafe fn apply_theme() {
    if let Some(window) = *PICKER_WINDOW.lock().unwrap() {
        apply_panel_appearance(window.0);
    }
    if let Some(window) = *DETAIL_WINDOW.lock().unwrap() {
        apply_panel_appearance(window.0);
    }
    apply_glass_properties();
    apply_clear_history_confirmation_theme();
    if let Some(buttons) = *CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap() {
        for button in buttons {
            set_clear_confirmation_button_style(button.0, false);
        }
    }
}

pub(super) unsafe fn ensure_picker_window() {
    if PICKER_WINDOW.lock().unwrap().is_some() {
        return;
    }
    let screen: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
    let screen_frame: NSRect = msg_send![screen, frame];
    let w = PICKER_W;
    // 初始高度按最大高度(占位;show_picker 每次按实际 pitch 重设)。
    // Initial height sized for the max height (placeholder; show_picker re-sizes per
    // summon using the real pitches).
    let h = PICKER_MAX_HEIGHT;
    let x = (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x;
    let y = (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y;
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));

    // NSPanel + NSWindowStyleMaskNonactivatingPanel(1<<7):成为 key 但不激活所属 app,
    // 与窗口切换浮窗一致,避免抢焦点。
    // NSPanel + NSWindowStyleMaskNonactivatingPanel (1<<7): becomes key WITHOUT activating
    // the owning app (same as the switcher overlay), so focus isn't stolen.
    let style: u64 = 1 << 7;

    let window_cls = {
        let name = CString::new("OhMyTabClipboardWindow").unwrap();
        let superclass = class!(NSPanel) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(canBecomeKeyWindow),
            picker_window_can_become_key as *mut c_void,
            types_bool.as_ptr(),
        );
        let types_event = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(sendEvent:),
            clipboard_window_send_event as *mut c_void,
            types_event.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    let window: *mut AnyObject = msg_send![window_cls, alloc];
    let window: *mut AnyObject = msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false];
    apply_panel_appearance(window);
    // 浮窗级 tracking area 需要持续收到 mouseMoved,才能在正文按钮之外的行内留白
    // 重新核对悬停行。
    // The picker-wide tracking area needs mouseMoved continuously so hover can be reconciled
    // while the pointer is over row padding outside the content buttons.
    let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
    let _: () = msg_send![window, setLevel: 3u64];
    let _: () = msg_send![window, setOpaque: false];
    let _: () = msg_send![window, setReleasedWhenClosed: false];
    // 背景与窗口切换浮窗同款:clearColor + 玻璃视图提供视觉效果(见下)。
    // Same backdrop as the switcher overlay: clearColor + a glass view for the visuals (below).
    let clear: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear];
    // 玻璃自带深度,窗口阴影是多余的(与窗口切换浮窗一致)。
    // The glass carries its own depth; the window shadow is redundant (same as the overlay).
    let _: () = msg_send![window, setHasShadow: false];

    // --- 玻璃背景(Liquid Glass),与窗口切换浮窗同款 ---
    // macOS 26+  → NSGlassEffectView(新公开 API,自带模糊)
    // macOS <26 → NSVisualEffectView(withinWindow + Dark material)
    // Glass backdrop (Liquid Glass), same as the switcher overlay:
    // macOS 26+ -> NSGlassEffectView (new public API, built-in blur)
    // macOS <26  -> NSVisualEffectView (withinWindow + Dark material).
    let is_macos_26 = AnyClass::get(c"NSGlassEffectView").is_some();
    // 容器将被加进的父视图 / the parent view the container is added into.
    let content_parent: *mut AnyObject;

    if is_macos_26 {
        let glass_cls = AnyClass::get(c"NSGlassEffectView").unwrap();
        let glass: *mut AnyObject = msg_send![glass_cls, alloc];
        let glass: *mut AnyObject =
            msg_send![glass, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        // 小浮窗固定小圆角(不跟随 config 的大圆角)。
        // Fixed small corner radius for this small panel (not the config's big one).
        let radius = CORNER_R;
        let _: () = msg_send![glass, setCornerRadius: radius];
        let style_i: i64 = match crate::config::effective_glass_style().as_str() {
            "clear" => 1,
            _ => 0,
        };
        let _: () = msg_send![glass, setStyle: style_i];
        let tint_hex = crate::config::parse_hex8(&crate::config::effective_glass_tint());
        let tint = crate::ffi::hex_to_ns_color(tint_hex);
        let _: () = msg_send![glass, setTintColor: tint];
        let _: () = msg_send![glass, setAutoresizingMask: 18u64];
        let _: () = msg_send![window, setContentView: glass];
        // NSGlassEffectView.contentView 初始可能为 nil,自建一个内层视图。
        // NSGlassEffectView.contentView may be nil initially - create our own.
        let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let inner: *mut AnyObject =
            msg_send![inner, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        let _: () = msg_send![inner, setAutoresizingMask: 18u64];
        let _: () = msg_send![glass, setContentView: inner];
        // 硬裁剪背景模糊进圆角(与窗口切换浮窗同款处理)。
        // Hard-clip the backdrop blur into the corner radius (same trick as the overlay).
        let _: () = msg_send![glass, setWantsLayer: true];
        let glass_layer: *mut AnyObject = msg_send![glass, layer];
        if !glass_layer.is_null() {
            let _: () = msg_send![glass_layer, setCornerRadius: radius];
            let _: () = msg_send![glass_layer, setMasksToBounds: true];
        }
        *PICKER_GLASS.lock().unwrap() = Some(ObjPtr::new(glass));
        content_parent = inner;
    } else {
        let content: *mut AnyObject = msg_send![window, contentView];
        let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
        let ve: *mut AnyObject =
            msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))];
        // withinWindow blending + Dark material(与窗口切换浮窗一致)。
        // withinWindow blending + Dark material (same as the switcher overlay).
        let _: () = msg_send![ve, setBlendingMode: 1u64]; // WithinWindow
        let _: () = msg_send![ve, setMaterial: 12u64]; // Dark
        let _: () = msg_send![ve, setState: 1u64]; // Active
        let _: () = msg_send![ve, setAutoresizingMask: 18u64];
        let _: () = msg_send![content, addSubview: ve];
        content_parent = ve;
    }

    *PICKER_CONTENT_PARENT.lock().unwrap() = Some(ObjPtr::new(content_parent));

    // 容器(接收键盘事件;flipped,行从顶部往下排,最新条目在顶)。
    // Container (receives key events; flipped so rows stack top-down, newest on top).
    let container = {
        let name = CString::new("OhMyTabClipboardContainer").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_key = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(keyDown:),
            container_key_down as *mut c_void,
            types_key.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseMoved:),
            container_mouse_moved as *mut c_void,
            types_key.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseEntered:),
            container_mouse_moved as *mut c_void,
            types_key.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseExited:),
            container_mouse_exited as *mut c_void,
            types_key.as_ptr(),
        );
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(acceptsFirstResponder),
            container_accepts_first_responder as *mut c_void,
            types_bool.as_ptr(),
        );
        // flipped:原点在左上,y 向下增长——行从顶部排起,最新在最上。
        // Flipped: origin at top-left, y grows downward -- rows stack from the top.
        class_addMethod(
            cls,
            sel!(isFlipped),
            container_is_flipped as *mut c_void,
            types_bool.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    let container: *mut AnyObject = msg_send![container, alloc];
    let container: *mut AnyObject = msg_send![
        container,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, h))
    ];
    // documentView 的高度由 rebuild_rows 按条目数动态设置,不跟随 scroll view 拉伸。
    // The document view's height is set dynamically by rebuild_rows; it must NOT stretch
    // with the scroll view.
    let _: () = msg_send![container, setAutoresizingMask: 0u64];
    add_picker_hover_tracking(content_parent, container);

    // 固定头部条:搜索框 + 清除按钮所在行,不随列表滚动(滚动时文字曾从半透明 tile
    // 底下穿过形成重叠)。flipped 坐标系让搜索框/清除按钮的既有 frame 直接可用。
    // A fixed header strip holding the search field + the clear button; it does NOT scroll
    // with the list (scrolling text used to bleed through the translucent tiles). Flipped so
    // the search/clear frames work unchanged.
    let header_strip: *mut AnyObject = {
        let name = CString::new("OhMyTabClipHeaderView").unwrap();
        let superclass = class!(NSView) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_bool = CString::new("B@:").unwrap();
        class_addMethod(
            cls,
            sel!(isFlipped),
            header_strip_is_flipped as *mut c_void,
            types_bool.as_ptr(),
        );
        objc_registerClassPair(cls);
        let strip: *mut AnyObject = msg_send![cls, alloc];
        let strip: *mut AnyObject = msg_send![
            strip,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, h - header_strip_h()),
                NSSize::new(w, header_strip_h())
            )
        ];
        // NSViewMinYMargin(8):底边距自适应 → 窗口高度变化时始终贴顶。
        // NSViewMinYMargin (8): the bottom gap adapts -> pinned to the top as the window
        // resizes.
        let _: () = msg_send![strip, setAutoresizingMask: 8u64];
        let _: () = msg_send![content_parent, addSubview: strip];
        release_obj(strip);
        strip
    };

    // NSScrollView:滚轮滚动 + 自定义滚动指示器(去掉系统滚动条,视觉更贴合玻璃)。
    // 只占头部条以下的区域:列表在自身区域内滚动,避免与搜索行重叠。
    // NSScrollView: wheel scrolling + a custom scroll indicator (the system scroller is
    // replaced for a cleaner look on the glass). It only occupies the area below the header
    // strip: the list scrolls within its own region and can never overlap the search row.
    let scroll: *mut AnyObject = msg_send![class!(NSScrollView), alloc];
    let scroll: *mut AnyObject = msg_send![
        scroll,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, FOOTER_H),
            NSSize::new(w, h - header_strip_h() - FOOTER_H)
        )
    ];
    let _: () = msg_send![scroll, setAutoresizingMask: 18u64];
    let _: () = msg_send![scroll, setBorderType: 0u64]; // NSNoBorder
    let _: () = msg_send![scroll, setDrawsBackground: false];
    let _: () = msg_send![scroll, setHasVerticalScroller: false];
    let _: () = msg_send![scroll, setHasHorizontalScroller: false];
    let _: () = msg_send![content_parent, addSubview: scroll];
    release_obj(scroll);
    let _: () = msg_send![scroll, setDocumentView: container];
    release_obj(container);

    // 自定义滚动指示器:右侧 4pt 宽胶囊条,半透明白,滚动时显示、停止 1s 后淡出。
    // Custom scroll indicator: a 4pt rounded capsule on the right, semi-transparent white;
    // shown while scrolling and faded out 1s after scrolling stops.
    //
    // 这里不能使用普通 NSView:系统滚动条已关闭,普通视图既不会响应拖拽也不会改变
    // NSScrollView 的 content offset,所以之前只能滚轮/键盘滚动。
    // A plain NSView is not enough here: with the system scroller disabled it neither handles
    // thumb dragging nor changes NSScrollView's content offset, which is why only wheel/key
    // scrolling worked before.
    let indicator: *mut AnyObject = msg_send![scroll_indicator_class(), alloc];
    let indicator: *mut AnyObject = msg_send![
        indicator,
        initWithFrame: NSRect::new(
            NSPoint::new(w - SCROLL_INDICATOR_HIT_W - 3.0, 3.0),
            NSSize::new(
                SCROLL_INDICATOR_HIT_W,
                h - header_strip_h() - FOOTER_H - 6.0,
            )
        )
    ];
    // 透明命中区域比可见胶囊更宽;不要把背景设到父层,否则会把 10pt 全部画出来。
    // The transparent hit area is wider than the visible capsule; do not paint the parent
    // layer or all 10pt would become visible.
    update_scroll_indicator_visual(indicator, h - header_strip_h() - FOOTER_H - 6.0, false);
    let _: () = msg_send![indicator, setHidden: true];
    let _: () = msg_send![scroll, addSubview: indicator];
    release_obj(indicator);

    // 观察 clipView 的 bounds 变化(滚动发生)→ 更新指示器 + 重启淡出计时器。
    // Observe the clip view's bounds changes (scrolling) -> update the indicator + restart
    // the fade-out timer.
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let _: () = msg_send![clip, setPostsBoundsChangedNotifications: true];
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let bounds_name = make_nsstring("NSViewBoundsDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(scrollIndicatorBoundsChanged:),
        name: bounds_name,
        object: clip
    ];
    CFRelease(bounds_name as *const c_void);
    *SCROLL_VIEW.lock().unwrap() = Some(ObjPtr::new(scroll));
    *SCROLL_INDICATOR.lock().unwrap() = Some(ObjPtr::new(indicator));

    // 顶部搜索框(NSSearchField 子类):模糊过滤条目。不自动聚焦(用户点击才开始搜索)。
    // 子类只重写 cancelOperation:(Esc)——编辑期间的按键由字段编辑器处理,↓ 等命令经
    // delegate 的 control:textView:doCommandBySelector: 拦截(见 search_field_do_command)。
    // Top search field (an NSSearchField subclass): fuzzy entry filtering. Not auto-focused
    // (the user clicks it to start searching). The subclass only overrides cancelOperation:
    // (Esc) -- while editing, keys go to the field editor, and commands like ↓ are intercepted
    // via the delegate's control:textView:doCommandBySelector: (see search_field_do_command).
    let search_cls = {
        let name = CString::new("OhMyTabClipSearchField").unwrap();
        let superclass = class!(NSSearchField) as *const _ as *mut AnyObject;
        let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
        let types_v = CString::new("v@:@").unwrap();
        class_addMethod(
            cls,
            sel!(cancelOperation:),
            search_field_cancel as *mut c_void,
            types_v.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(clearSearch:),
            search_clear_button as *mut c_void,
            types_v.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseDown:),
            search_field_mouse_down as *mut c_void,
            types_v.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseMoved:),
            search_field_mouse_moved as *mut c_void,
            types_v.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseEntered:),
            search_field_mouse_entered as *mut c_void,
            types_v.as_ptr(),
        );
        class_addMethod(
            cls,
            sel!(mouseExited:),
            search_field_mouse_exited as *mut c_void,
            types_v.as_ptr(),
        );
        objc_registerClassPair(cls);
        cls
    };
    // 搜索框(设计稿 .search):48pt 高、10 圆角、4.5% 黑底 + 1px 内描边,占位左对齐。
    // The search field (the mockup's .search): 48pt tall, radius 10, a 4.5% black fill
    // with a 1px inner ring; the placeholder is left-aligned.
    let search_w = PICKER_W - SEARCH_PAD_X * 2.0;
    let search: *mut AnyObject = msg_send![search_cls, alloc];
    let search: *mut AnyObject = msg_send![
        search,
        initWithFrame: NSRect::new(
            NSPoint::new(SEARCH_PAD_X, TOP_PAD_Y),
            NSSize::new(search_w, SEARCH_H)
        )
    ];
    // 自定义 cell:占位 = "放大镜 SF Symbol + 搜索提示"整体画在字段左侧(见
    // search_cell_class),⌘F 键帽画在最右侧。
    // A custom cell: the placeholder = "magnifier SF Symbol + search hint" drawn at the
    // field's left (see search_cell_class), with the ⌘F keycap at the far right.
    let cell: *mut AnyObject = msg_send![search_cell_class(), alloc];
    let empty_ns = make_nsstring("");
    let cell: *mut AnyObject = msg_send![cell, initTextCell: empty_ns];
    CFRelease(empty_ns as *const c_void);
    // NSSearchField 的 field editor 不保证从 control 继承字号;先把 cell 固定为共享
    // 字号,并在 selectWithFrame: 中再次应用给实际 editor。
    // NSSearchField's field editor does not reliably inherit the control font; set the cell to
    // the shared size here and apply it again to the live editor in selectWithFrame:.
    let search_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: SEARCH_FONT_SIZE];
    let _: () = msg_send![cell, setFont: search_font];
    // 占位提示(放大镜 + 文案)独立构建;按设计稿为静态文案(条数显示移到底部栏)。
    // The placeholder (magnifier + text) is built separately; per the mockup it is a
    // static string (the entry count moved to the footer).
    rebuild_search_hint();
    // 保留原生 search button 的布局宽度,但清除它的图像;所有状态统一由 cell 手绘
    // 初始的 ⌕，否则输入时 AppKit 会换成不同的 stock magnifier。
    // Keep the native search button's layout width but clear its image; the cell hand-draws the
    // original ⌕ in every state, preventing AppKit from substituting a different stock magnifier
    // while typing.
    let search_button: *mut AnyObject = msg_send![cell, searchButtonCell];
    if !search_button.is_null() {
        let blank_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let blank_image: *mut AnyObject =
            msg_send![blank_image, initWithSize: NSSize::new(22.0, 22.0)];
        let _: () = msg_send![search_button, setImage: blank_image];
        release_obj(blank_image);
    }
    let _: () = msg_send![search, setCell: cell];
    // NSSearchFieldCell 默认把 × 的 cancelOperation: 发往响应链,字段编辑器可能吞掉
    // 它。将 cancel cell 直连到字段的 clearSearch:，确保点击一定同步清空过滤条件。
    // NSSearchFieldCell normally sends its × cancelOperation: through the responder chain,
    // where the field editor can consume it. Bind it directly to clearSearch: so a click always
    // clears the matching filter too.
    let cancel_cell: *mut AnyObject = msg_send![cell, cancelButtonCell];
    if !cancel_cell.is_null() {
        // 透明原生 cancel image 仍保留其布局/事件兼容性;可见的 × 由 cell 按 HTML
        // 尺寸绘制,因此不会和系统符号混用。
        // Keep the native cancel image transparent for layout/event compatibility; the cell
        // draws the visible × at the HTML size so no system symbol is mixed in.
        let blank_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let blank_image: *mut AnyObject =
            msg_send![blank_image, initWithSize: NSSize::new(SEARCH_CLEAR_W, SEARCH_CLEAR_W)];
        let _: () = msg_send![cancel_cell, setImage: blank_image];
        release_obj(blank_image);
        let _: () = msg_send![cancel_cell, setTarget: search];
        let _: () = msg_send![cancel_cell, setAction: sel!(clearSearch:)];
    }
    release_obj(cell);
    // 显式置空 placeholder 属性(双保险,任何读取方都拿不到内容)。
    // Explicitly empty the placeholder property (belt and braces; no reader finds text).
    let empty_attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let empty_attr: *mut AnyObject = msg_send![empty_attr, init];
    let _: () = msg_send![search, setPlaceholderAttributedString: empty_attr];
    release_obj(empty_attr);
    // 修复:initTextCell: 创建的自定义 cell 默认不可编辑(isEditable=false),
    // NSSearchField 因此 acceptsFirstResponder=false——点击/↑ 都无法进入编辑。
    // 替换 cell 后必须显式恢复 editable(selectable 一并保证)。
    // FIX: a custom cell created via initTextCell: is NOT editable by default
    // (isEditable=false), which makes NSSearchField refuse first responder -- clicks and
    // the ↑ jump could never start editing. Editable must be restored explicitly after
    // replacing the cell (selectable too, for good measure).
    let _: () = msg_send![search, setEditable: true];
    let _: () = msg_send![search, setSelectable: true];
    // 聚焦环会在编辑时画一圈方形描边,破坏圆角观感——关闭。
    // The focus ring draws a square outline while editing, breaking the rounded look --
    // disabled.
    let _: () = msg_send![search, setFocusRingType: 1u64]; // NSFocusRingTypeNone
                                                           // 编辑态文本与占位一致左对齐(设计稿文字靠左)。
                                                           // Editing text is left-aligned like the placeholder (the mockup's layout).
    let _: () = msg_send![search, setAlignment: 0u64]; // left
                                                       // 输入态必须与 ↓ 后手绘的保留查询统一为 14pt,避免焦点切换时字号突变。
                                                       // Match the 14pt hand-drawn retained query after ↓, avoiding a font-size jump on focus change.
    let _: () = msg_send![search, setFont: search_font];
    // 磨砂化:去掉系统描边/bezel,换成共享 field surface + 1px 内描边;保留的系统 ×
    // 已直连 clearSearch:，不会因响应链而失效。
    // Frosted: drop the system bezel and use the shared field surface/ring; the remaining
    // system × is bound directly to clearSearch:, not the responder chain.
    let _: () = msg_send![search, setBezeled: false];
    let _: () = msg_send![search, setDrawsBackground: false];
    let _: () = msg_send![search, setWantsLayer: true];
    let search_layer: *mut AnyObject = msg_send![search, layer];
    style_search_field(search, false);
    let _: () = msg_send![search_layer, setBorderWidth: 1.0f64];
    let _: () = msg_send![search_layer, setCornerRadius: SEARCH_R];
    // delegate = observer()(复用通知单例):↓ 命令拦截(字段编辑器转发 moveDown:)。
    // Delegate = observer() (reusing the notification singleton): intercepts ↓ (the field
    // editor forwards moveDown:).
    let _: () = msg_send![search, setDelegate: observer()];
    // 搜索框挂在固定头部条(不随列表滚动)。
    // The search field lives in the fixed header strip (it does not scroll with the list).
    let _: () = msg_send![header_strip, addSubview: search];
    // 用 tracking area 只追踪搜索框内的 ×;InVisibleRect 让 AppKit 在尺寸变化时自动更新范围。
    // Track only the search field's × with a tracking area; InVisibleRect lets AppKit update
    // its range automatically if the field is resized.
    let tracking: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let tracking: *mut AnyObject = msg_send![
        tracking,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(search_w, SEARCH_H)),
        // NSTrackingArea 必须指定一个 active 状态;非激活的浮窗需要 ActiveAlways。
        // NSTrackingArea requires one active state; this nonactivating panel needs ActiveAlways.
        options: 0x283u64, // entered/exited + moved + active-always + in-visible-rect
        owner: search,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![search, addTrackingArea: tracking];
    release_obj(tracking);
    // 自绘 × 没有原生 cell 的点击目标;叠放一个透明 NSButton 确保它先收到 click,
    // 避免 NSSearchFieldCell 吞掉 mouseDown。文字与悬停底仍由下面的 cell 统一绘制。
    // The hand-drawn × has no native click target, so overlay a transparent NSButton that gets
    // the click before NSSearchFieldCell can consume mouseDown. The cell below still draws its
    // glyph and hover fill consistently.
    let clear_button: *mut AnyObject = msg_send![class!(NSButton), alloc];
    let clear_button: *mut AnyObject = msg_send![
        clear_button,
        initWithFrame: NSRect::new(
            NSPoint::new(
                SEARCH_PAD_X + search_w - SEARCH_PAD_IN - SEARCH_CLEAR_W,
                TOP_PAD_Y + (SEARCH_H - ACTION_H) / 2.0
            ),
            NSSize::new(SEARCH_CLEAR_W, ACTION_H)
        )
    ];
    let _: () = msg_send![clear_button, setBordered: false];
    let empty_title = make_nsstring("");
    let _: () = msg_send![clear_button, setTitle: empty_title];
    CFRelease(empty_title as *const c_void);
    let _: () = msg_send![clear_button, setTarget: search];
    let _: () = msg_send![clear_button, setAction: sel!(clearSearch:)];
    let _: () = msg_send![clear_button, setHidden: true];
    let _: () = msg_send![header_strip, addSubview: clear_button];
    release_obj(clear_button);
    *SEARCH_CLEAR_BUTTON.lock().unwrap() = Some(ObjPtr::new(clear_button));
    release_obj(search);
    *SEARCH_FIELD.lock().unwrap() = Some(ObjPtr::new(search));
    // 文本变化(含系统清除按钮/NSSearchField 的 Esc 清空)→ 实时过滤。
    // Text changes (including the system clear button / NSSearchField's Esc clear) filter live.
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let text_name = make_nsstring("NSControlTextDidChangeNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFieldChanged:),
        name: text_name,
        object: search
    ];
    CFRelease(text_name as *const c_void);
    // 聚焦样式:保持磨砂底色,仅加深内描边;失焦还原。
    // Focus style: preserve the frosted fill and strengthen only the inner ring; restore it
    // on blur.
    let begin_name = make_nsstring("NSControlTextDidBeginEditingNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFocusBegan:),
        name: begin_name,
        object: search
    ];
    CFRelease(begin_name as *const c_void);
    let end_name = make_nsstring("NSControlTextDidEndEditingNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(searchFocusEnded:),
        name: end_name,
        object: search
    ];
    CFRelease(end_name as *const c_void);

    // 筛选行(设计稿 .filters):纯文字 12pt,选中项加深 + 底部 16×2 下划线。
    // The filters row (the mockup's .filters): bare 12pt text; the active one darkens and
    // gains a 16x2 underline.
    let filter_labels = localized_filter_labels();
    let filters_y = picker_filters_y();
    *FILTER_PILLS.lock().unwrap() = Vec::new();
    let mut fx = FILTERS_PAD_X;
    for (i, lab) in filter_labels.iter().enumerate() {
        // 按钮宽 = 文字宽 + 点击余量;间距按设计稿 17px。
        // Button width = the text + click slack; gaps per the mockup's 17px.
        let w = localized_string_width(lab, 12.0) + 12.0;
        let pill = make_filter_pill(lab, i as isize, fx, filters_y, w);
        let _: () = msg_send![header_strip, addSubview: pill];
        release_obj(pill);
        FILTER_PILLS.lock().unwrap().push(ObjPtr::new(pill));
        fx += w + FILTER_GAP;
    }
    update_filter_pill_style(false);

    // 清空历史:筛选行右侧始终显示两个紧凑文字按钮,不再先展开确认卡片。
    // Clear history: keep two compact text actions visible beside the filters instead of
    // expanding a separate confirmation card first.
    let clear_labels = [
        t("clipboard.clear_confirm_unpinned"),
        t("clipboard.clear_confirm_all"),
    ];
    let clear_actions = [sel!(clearClipboardUnpinned:), sel!(clearClipboardAll:)];
    let clear_widths: [f64; 2] =
        std::array::from_fn(|i| localized_string_width(&clear_labels[i], 12.0) + 8.0);
    let clear_total_w = clear_widths.iter().sum::<f64>() + CLEAR_CONFIRM_GAP;
    let mut clear_x = PICKER_W - SEARCH_PAD_X - clear_total_w;
    let mut clear_buttons = [std::ptr::null_mut(); 2];
    for i in 0..2 {
        let frame = NSRect::new(
            NSPoint::new(clear_x, filters_y + 8.0),
            NSSize::new(clear_widths[i], 20.0),
        );
        let button: *mut AnyObject = msg_send![hover_button_class(), alloc];
        let button: *mut AnyObject = msg_send![button, initWithFrame: frame];
        let _: () = msg_send![button, setBordered: false];
        let _: () = msg_send![button, setWantsLayer: true];
        let button_layer: *mut AnyObject = msg_send![button, layer];
        if !button_layer.is_null() {
            let _: () = msg_send![button_layer, setCornerRadius: 5.0f64];
        }
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
        let _: () = msg_send![button, setFont: font];
        let title = make_nsstring(&clear_labels[i]);
        let _: () = msg_send![button, setTitle: title];
        CFRelease(title as *const c_void);
        let _: () = msg_send![button, setTarget: observer()];
        let _: () = msg_send![button, setAction: clear_actions[i]];
        set_clear_confirmation_button_style(button, false);
        add_hover_tracking(button);
        let _: () = msg_send![header_strip, addSubview: button];
        release_obj(button);
        clear_buttons[i] = button;
        clear_x += clear_widths[i] + CLEAR_CONFIRM_GAP;
    }
    *CLEAR_HISTORY_BUTTON.lock().unwrap() = None;
    *CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap() =
        Some([ObjPtr::new(clear_buttons[0]), ObjPtr::new(clear_buttons[1])]);

    // 底部栏(新设计稿 .footer):43pt,顶部分隔线 + 条目数 + 快捷键图例(清空已移到
    // 筛选行)。/ The footer: a top hairline + the entry count + the shortcut legends
    // (clear history now lives in the filters row).
    build_footer(content_parent, w);
    // toast 标签(新设计稿 .toast):暗底白字圆角胶囊,底部居中,置于 footer 之上。
    // The toast label (the new mockup's .toast): a dark rounded pill at the bottom center,
    // above the footer.
    let toast_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let toast_label: *mut AnyObject = msg_send![
        toast_label,
        initWithFrame: NSRect::new(
            NSPoint::new(200.0, 22.0),
            NSSize::new(120.0, 26.0)
        )
    ];
    let _: () = msg_send![toast_label, setBezeled: false];
    let _: () = msg_send![toast_label, setDrawsBackground: false];
    let _: () = msg_send![toast_label, setEditable: false];
    let _: () = msg_send![toast_label, setSelectable: false];
    let _: () = msg_send![toast_label, setAlignment: 1isize]; // Center on arm64
    let tf: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
    let _: () = msg_send![toast_label, setFont: tf];
    let white: *mut AnyObject = msg_send![class!(NSColor), whiteColor];
    let _: () = msg_send![toast_label, setTextColor: white];
    let _: () = msg_send![toast_label, setWantsLayer: true];
    let tlayer: *mut AnyObject = msg_send![toast_label, layer];
    let tbg: *mut AnyObject =
        msg_send![class!(NSColor), colorWithWhite: 30.0f64 / 255.0, alpha: 0.86f64];
    crate::ffi::layer_set_background(tlayer, crate::ffi::ns_color_to_cg(tbg));
    let _: () = msg_send![tlayer, setCornerRadius: 7.0f64];
    let _: () = msg_send![toast_label, setHidden: true];
    let _: () = msg_send![content_parent, addSubview: toast_label];
    release_obj(toast_label);
    *TOAST_LABEL.lock().unwrap() = Some(ObjPtr::new(toast_label));

    // 点击外部(浮窗失去 key)→ 自动隐藏。Win+V 同款行为:呼出后点任何地方即消失。
    // Outside clicks (the picker resigns key) -> auto-hide. Same as Win+V: any click after
    // summoning dismisses the picker.
    let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
    let resign_name = make_nsstring("NSWindowDidResignKeyNotification");
    let _: () = msg_send![
        center,
        addObserver: observer(),
        selector: sel!(clipboardWindowResigned:),
        name: resign_name,
        object: window
    ];
    CFRelease(resign_name as *const c_void);
    *PICKER_CONTAINER.lock().unwrap() = Some(ObjPtr::new(container));
    *PICKER_WINDOW.lock().unwrap() = Some(ObjPtr::new(window));
}

/// 建行期间累计的计时/计数,供慢路径日志使用(与原有统计口径一致)。
/// Per-row build counters accumulated for the slow-path log (same accounting as before).
#[derive(Default, Clone, Copy)]
struct RowCreateStats {
    image_rows: usize,
    code_rows: usize,
    image_ms: u128,
    content_attributed_ms: u128,
    meta_ms: u128,
}

/// 单行的创建参数(把众多行级参数打包,避免 `create_row_views` 参数过多)。
/// The per-row creation parameters (grouped so `create_row_views` stays readable).
struct RowSpec<'a> {
    entry: &'a ClipEntry,
    display_index: usize,
    y: f64,
    row_h: f64,
    has_header: bool,
    selected: bool,
    hovered: bool,
    show_source: bool,
    detail_open: bool,
    sel_idx: usize,
}

/// 创建单行(可选分组头 + 底块 + 内容/meta 按钮 + 3 个操作按钮),挂到 container 并返回
/// 其悬停相关视图。自 `rebuild_rows` 抽出:滚动时的增量物化(`sync_visible_rows`)复用它,
/// 只新建新进入视口的行,不再整组拆建。
///
/// Create one row (optional group header + tile + content/meta buttons + 3 action buttons),
/// attach it to the container, and return its hover-related views. Extracted from
/// `rebuild_rows` so scroll-time incremental materialization (`sync_visible_rows`) can reuse
/// it and create only the rows newly entering the viewport instead of tearing down and
/// rebuilding the whole visible set.
unsafe fn create_row_views(
    container: *mut AnyObject,
    spec: RowSpec<'_>,
    stats: &mut RowCreateStats,
) -> RowHoverViews {
    let RowSpec {
        entry,
        display_index,
        y,
        row_h,
        has_header,
        selected,
        hovered,
        show_source,
        detail_open,
        sel_idx,
    } = spec;
    let i = display_index;
    let palette = clipboard_palette();
    let row_w = PICKER_W - PAD_X * 2.0;
    let hdr_h = if has_header { GROUP_H } else { 0.0 };
    let content_y = y + hdr_h;

    // 分组头:一行 11px medium 小字(新设计稿 .group-title,27px 高,垂直居中,
    // 左内边距 13)。/ The group header: 11px medium text, 27px tall, centered.
    let mut row_group_label = None;
    if has_header {
        let g_label = make_nsstring(&group_label(day_group(entry.copied_at)));
        let g: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let g: *mut AnyObject = msg_send![
            g,
            initWithFrame: NSRect::new(
                NSPoint::new(PAD_X + ROW_PAD_L, y + GROUP_LABEL_PAD),
                NSSize::new(row_w - PAD_X, GROUP_H - GROUP_LABEL_PAD)
            )
        ];
        let _: () = msg_send![g, setStringValue: g_label];
        CFRelease(g_label as *const c_void);
        let _: () = msg_send![g, setBezeled: false];
        let _: () = msg_send![g, setDrawsBackground: false];
        let _: () = msg_send![g, setEditable: false];
        let _: () = msg_send![g, setSelectable: false];
        let g_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 12.0f64, weight: 0.23f64]; // Medium
        let _: () = msg_send![g, setFont: g_font];
        let g_color = crate::ffi::hex_to_ns_color(palette.muted_text);
        let _: () = msg_send![g, setTextColor: g_color];
        let _: () = msg_send![container, addSubview: g];
        release_obj(g);
        row_group_label = Some(ObjPtr::new(g));
    }

    // 行底(两种不同样式):悬停(未选中)= 0.032 黑(**没有**左条);选中 = 0.050 黑 +
    // 2px 左指示条。按新设计稿 .item:hover vs .item.selected。
    // The row backdrop (two distinct styles): hovered (not selected) = 0.032 black
    // with NO bar; selected = 0.050 black + a 2px left bar. The new mockup's
    // .item:hover vs .item.selected.
    let tile: *mut AnyObject = msg_send![class!(NSView), alloc];
    let tile: *mut AnyObject = msg_send![
        tile,
        initWithFrame: NSRect::new(NSPoint::new(PAD_X, content_y), NSSize::new(row_w, row_h))
    ];
    let _: () = msg_send![tile, setWantsLayer: true];
    let tile_layer: *mut AnyObject = msg_send![tile, layer];
    let bg_hex = if selected {
        palette.selection_bg
    } else if hovered {
        palette.hover_bg
    } else {
        0x00000000
    };
    // layer_set_background 走 raw objc_msgSend:objc2 的 msg_send! 无法编码
    // CGColor 参数/返回(参数编码 '^{CGColor=}' 与 *mut c_void 的 '^v' 不匹配)。
    // layer_set_background goes through raw objc_msgSend: objc2's msg_send! can't encode
    // CGColor args/returns ('^{CGColor=}' vs '^v').
    crate::ffi::layer_set_background(tile_layer, crate::ffi::hex_to_cg_color(bg_hex));
    let _: () = msg_send![tile_layer, setCornerRadius: SEL_TILE_R];
    // 每行都预建左侧 2px 指示条并按选中状态隐藏,这样方向键切换只需切换可见性。
    // Prebuild the 2px selection bar for every row and hide it when unselected, so arrow
    // navigation only toggles visibility instead of rebuilding rows.
    let bar: *mut AnyObject = msg_send![class!(NSView), alloc];
    let bar: *mut AnyObject = msg_send![
        bar,
        initWithFrame: NSRect::new(
            NSPoint::new(SEL_BAR_X, SEL_BAR_INSET_Y),
            NSSize::new(SEL_BAR_W, row_h - SEL_BAR_INSET_Y * 2.0)
        )
    ];
    let _: () = msg_send![bar, setWantsLayer: true];
    let bar_layer: *mut AnyObject = msg_send![bar, layer];
    crate::ffi::layer_set_background(bar_layer, crate::ffi::hex_to_cg_color(palette.accent));
    let _: () = msg_send![bar_layer, setCornerRadius: SEL_BAR_W / 2.0];
    let _: () = msg_send![bar, setHidden: !selected];
    let _: () = msg_send![tile, addSubview: bar];
    release_obj(bar);
    let _: () = msg_send![container, addSubview: tile];
    release_obj(tile);

    // 内容按钮:占行的上部(61pt),整块可点击(粘贴)+ 悬停;图片行左侧是 72×44
    // 缩略图画布 + 文件名;文本行是 ≤2 行、按类型着色的内容。无边框、无背景。
    // The content button: the row's upper zone (61pt), clickable (paste) + hover;
    // image rows get a 72x44 thumbnail canvas + the filename; text rows show <=2
    // styled lines. Borderless, backgroundless.
    let content_x = PAD_X + ROW_PAD_L;
    let content_w = row_w - ROW_PAD_L - ROW_PAD_R;
    let content_h = row_h - META_FOOTER_H; // 底部留给 meta 栏 / the meta bar takes the bottom.
    let content_btn: *mut AnyObject = msg_send![row_button_class(), alloc];
    let is_image = entry.image.is_some();
    if is_image {
        stats.image_rows += 1;
    }
    let content_btn: *mut AnyObject = msg_send![
        content_btn,
        initWithFrame: NSRect::new(
            NSPoint::new(content_x, content_y + ROW_PAD_TOP),
            NSSize::new(content_w, content_h - ROW_PAD_TOP - ROW_PAD_BOT)
        )
    ];
    let _: () = msg_send![content_btn, setBordered: false];
    let _: () = msg_send![content_btn, setAlignment: -1isize]; // NSTextAlignmentNatural
    let cell: *mut AnyObject = msg_send![content_btn, cell];
    let _: () = msg_send![cell, setUsesSingleLineMode: false];
    let _: () = msg_send![cell, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    if msg_send![cell, respondsToSelector: sel!(setMaximumNumberOfLines:)] {
        let _: () = msg_send![cell, setMaximumNumberOfLines: 2isize];
    }
    let image_started = Instant::now();
    let row_img = make_row_image(entry);
    stats.image_ms += image_started.elapsed().as_millis();
    if !row_img.is_null() {
        let _: () = msg_send![content_btn, setImage: row_img];
        let _: () = msg_send![content_btn, setImagePosition: 2isize]; // NSImageLeft
        release_obj(row_img);
    }
    // Keep the full string and let the native cell wrap/truncate using actual font metrics.
    // Character-count heuristics break on emoji, combining marks, and long unbroken words.
    // 保留完整字符串，让原生 cell 按实际字体测量换行/截断；字符数启发式会错误处理
    // emoji、组合字符和无空格长单词。
    let content = entry.text.as_str();
    let kind = if is_image {
        TextKind::Plain
    } else {
        classify_text(&entry.text)
    };
    if kind == TextKind::Code {
        stats.code_rows += 1;
    }
    let content_attributed_started = Instant::now();
    let attr = make_content_attributed(content, kind);
    stats.content_attributed_ms += content_attributed_started.elapsed().as_millis();
    let _: () = msg_send![content_btn, setAttributedTitle: attr];
    release_obj(attr);
    let _: () = msg_send![content_btn, setTag: i as isize];
    let _: () = msg_send![content_btn, setTarget: row_target()];
    let _: () = msg_send![content_btn, setAction: sel!(handleClipboardRowClick:)];
    add_hover_tracking(content_btn);
    let _: () = msg_send![container, addSubview: content_btn];
    release_obj(content_btn);

    // 底部 meta 按钮:17pt 栏,左侧是 [13px 来源图标]·应用名·时间,整块可可点
    // (点击 = 粘贴)、悬停选中;右侧悬浮着操作按钮。
    // 位置 = 行底向上留 ROW_PAD_BOT(8pt,对应设计稿 .item 的 padding-bottom 8px)
    // —— 之前贴行底,meta 栏与删除/详情/收藏按钮离下边框太近。
    // The bottom meta button: a 17pt bar with [13px source icon] + app · time on the
    // left; clickable (paste) and hover-tracked; the action buttons float on its right.
    // Positioned ROW_PAD_BOT (8pt) above the row bottom, matching the mockup's
    // .item padding-bottom 8px -- it used to sit flush with the bottom edge, leaving
    // the meta bar and the delete/details/pin buttons too close to the bottom border.
    let meta_y = content_y + row_h - META_FOOTER_H - ROW_PAD_BOT;
    let meta_btn: *mut AnyObject = msg_send![row_button_class(), alloc];
    let meta_w = row_w - ROW_PAD_L - ROW_PAD_R - ACTIONS_W - 4.0;
    let meta_btn: *mut AnyObject = msg_send![
        meta_btn,
        initWithFrame: NSRect::new(
            NSPoint::new(content_x, meta_y),
            NSSize::new(meta_w, META_FOOTER_H)
        )
    ];
    let _: () = msg_send![meta_btn, setBordered: false];
    let _: () = msg_send![meta_btn, setAlignment: -1isize]; // NSTextAlignmentNatural
    let mcell: *mut AnyObject = msg_send![meta_btn, cell];
    let _: () = msg_send![mcell, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let meta_started = Instant::now();
    let meta_attr = make_meta_footer_attributed(entry, show_source);
    stats.meta_ms += meta_started.elapsed().as_millis();
    let _: () = msg_send![meta_btn, setAttributedTitle: meta_attr];
    release_obj(meta_attr);
    let _: () = msg_send![meta_btn, setTag: i as isize];
    let _: () = msg_send![meta_btn, setTarget: row_target()];
    let _: () = msg_send![meta_btn, setAction: sel!(handleClipboardRowClick:)];
    add_hover_tracking(meta_btn);
    let _: () = msg_send![container, addSubview: meta_btn];
    release_obj(meta_btn);

    // 操作按钮(置顶 ☆/★ · 详情 ⓘ · 删除 ⌫):**置顶条目常显**,非置顶条目仅
    // 悬停/选中时显现(设计稿 .actions opacity 0→1)。独立于内容/meta 按钮,点击
    // 不触发粘贴。
    // Action buttons (pin ☆/★ · details ⓘ · delete ⌫): ALWAYS visible on PINNED
    // entries; on unpinned entries they appear only when the row is hovered or
    // selected (the mockup's .actions opacity 0->1). Separate from the content/meta
    // buttons; they never paste.
    let act_alpha = if entry.pinned || selected || hovered {
        1.0
    } else {
        0.0
    };
    let act_y = meta_y + (META_FOOTER_H - ACTION_H) / 2.0;
    let x_del = PICKER_W - PAD_X - ROW_PAD_R - ACTION_BTN;
    let x_details = x_del - ACTION_GAP - ACTION_BTN;
    let x_pin = x_details - ACTION_GAP - ACTION_BTN;
    let pin_sym = if entry.pinned { "★" } else { "☆" };
    let pin_btn = make_action_button(
        pin_sym,
        sel!(togglePin:),
        i as isize,
        x_pin,
        act_y,
        act_alpha,
    );
    if !pin_btn.is_null() {
        let _: () = msg_send![container, addSubview: pin_btn];
        release_obj(pin_btn);
    }
    let details_btn = make_action_button(
        "ⓘ",
        sel!(showItemDetails:),
        i as isize,
        x_details,
        act_y,
        act_alpha,
    );
    // 详情已展开且本行被选中时,详情按钮显示激活图标与独立圆角底。
    // When detail is open for this selected row, show its active icon and own rounded fill.
    set_detail_action_style(
        details_btn,
        detail_action_is_active(detail_open, sel_idx, i),
        false,
    );
    if !details_btn.is_null() {
        let _: () = msg_send![container, addSubview: details_btn];
        release_obj(details_btn);
    }
    let del_btn = make_action_button("⌫", sel!(deleteEntry:), i as isize, x_del, act_y, act_alpha);
    if !del_btn.is_null() {
        let _: () = msg_send![container, addSubview: del_btn];
        release_obj(del_btn);
    }

    RowHoverViews {
        group_label: row_group_label,
        tile: ObjPtr::new(tile),
        bar: ObjPtr::new(bar),
        content: ObjPtr::new(content_btn),
        meta: ObjPtr::new(meta_btn),
        pin: ObjPtr::new(pin_btn),
        details: ObjPtr::new(details_btn),
        del: ObjPtr::new(del_btn),
    }
}

/// 从容器移除一行的全部视图(bar 是 tile 的子视图,随 tile 一起释放)。
/// Remove one row's views from the container (the bar is a tile subview and goes with it).
unsafe fn remove_row_views(view: &RowHoverViews) {
    for v in [
        view.group_label,
        Some(view.tile),
        Some(view.content),
        Some(view.meta),
        Some(view.pin),
        Some(view.details),
        Some(view.del),
    ]
    .into_iter()
    .flatten()
    {
        if !v.0.is_null() {
            let _: () = msg_send![v.0, removeFromSuperview];
        }
    }
}

/// 按索引插入已物化的行(ROW_VIEW_INDICES 与 ROW_HOVER_VIEWS 并列且升序)。
/// 锁顺序与 `row_view_for_display_index` 一致:先 indices,再 views。
/// Insert a materialized row by index (ROW_VIEW_INDICES and ROW_HOVER_VIEWS stay parallel
/// and ascending). Lock order matches `row_view_for_display_index`: indices before views.
fn insert_materialized_row(index: usize, view: RowHoverViews) {
    let mut indices = ROW_VIEW_INDICES.lock().unwrap();
    let pos = indices.partition_point(|&i| i < index);
    if pos < indices.len() && indices[pos] == index {
        return;
    }
    let mut views = ROW_HOVER_VIEWS.lock().unwrap();
    indices.insert(pos, index);
    views.insert(pos, view);
}

/// 移除某个已物化行并释放其视图;不存在时无操作。
/// Remove a materialized row and release its views; no-op when absent.
unsafe fn remove_materialized_row(index: usize) {
    let view = {
        let mut indices = ROW_VIEW_INDICES.lock().unwrap();
        let Some(pos) = indices.iter().position(|&i| i == index) else {
            return;
        };
        indices.remove(pos);
        let mut views = ROW_HOVER_VIEWS.lock().unwrap();
        views.remove(pos)
    };
    remove_row_views(&view);
}

/// 根据当前历史重建行按钮(选中行高亮 + 圆角背景块)。
/// Rebuild the row buttons from history (selected row highlighted with a rounded tile).
pub(super) unsafe fn rebuild_rows() -> Option<PickerTimingSummary> {
    let rebuild_started = Instant::now();
    let hist = CLIP_HISTORY.lock().unwrap();
    let container = picker_container_ptr()?;
    // 重建会拆除旧行,期间的 enter/exit 事件会被门控;先丢弃旧索引,避免它落到新行。
    // Rebuild tears down the old rows and gates enter/exit events; discard the old index first
    // so it cannot land on an unrelated new row.
    *HOVER_ROW.lock().unwrap() = NO_SELECTION;
    // 重建期间忽略 mouseEntered(见 REBUILDING 注释)。
    // Ignore mouseEntered during the rebuild (see the REBUILDING note).
    REBUILDING.store(true, Ordering::SeqCst);
    // 记录当前滚动位置(flipped 坐标下,clipView.bounds.origin.y 即滚动偏移),
    // 重建后恢复——悬停/方向键 rebuild 不会把视口弹回顶部。
    // Record the current scroll offset (the clip view's bounds origin y in flipped coords)
    // and restore it after the rebuild, so hover/arrow rebuilds don't snap the viewport.
    let scroll_offset = {
        let clip: *mut AnyObject = msg_send![container, superview];
        if clip.is_null() {
            0.0
        } else {
            let b: NSRect = msg_send![clip, bounds];
            b.origin.y
        }
    };

    // 移除旧行 / remove old rows.
    // 注意:按钮 alloc +1 已在 addSubview 后 release(由父视图持有);
    // removeFromSuperview 会让父视图释放引用(计数归零、对象 dealloc),不应
    // 再对其 release——否则二次释放 use-after-free(曾导致第二次呼出 segfault)。
    // Note: the button's alloc +1 was released after addSubview (owned by the parent view);
    // removeFromSuperview drops the parent's reference (refcount hits zero, object deallocs),
    // so it must NOT be released again -- a second release was a use-after-free that crashed
    // on the second summon.
    let remove_old_started = Instant::now();
    // 行视图由父视图持有,removeFromSuperview 即释放,不应二次 release(曾经的 UAF 教训)。
    // Row views are parent-owned; removeFromSuperview releases them and they must never be
    // released again (a past UAF lesson).
    {
        let old_views = ROW_HOVER_VIEWS.lock().unwrap().clone();
        for view in &old_views {
            remove_row_views(view);
        }
    }
    ROW_HOVER_VIEWS.lock().unwrap().clear();
    ROW_VIEW_INDICES.lock().unwrap().clear();
    if let Some(empty) = EMPTY_STATE_VIEW.lock().unwrap().take() {
        if !empty.0.is_null() {
            let _: () = msg_send![empty.0, removeFromSuperview];
        }
    }
    let mut pitches = ROW_PITCHES.lock().unwrap();
    pitches.clear();
    let remove_old_ms = remove_old_started.elapsed().as_millis();

    let prepare_started = Instant::now();
    // 每行的按钮高/行距由文本换行行数决定。
    // Each row's button height / pitch derives from its wrapped line count.
    *pitches = compute_pitches(&hist);
    let total = hist.len();
    // 底部栏条目数随历史变化刷新(占位提示已改为静态文案)。
    // The footer's entry count follows the history (the search placeholder is now a
    // static string).
    refresh_footer_count(total);
    // 重建当前显示列表(按搜索词 + 筛选项过滤)。
    // Rebuild the display list (filtered by the query AND the kind filter).
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let filter = *CLIP_FILTER.lock().unwrap();
    let filtered_indices = filtered_indices(&hist, &query, filter);
    with_clipboard_ui(|ui| ui.filtered = filtered_indices);
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    let show_source = show_source_app();

    // 删除/裁剪后把选中索引钳到新显示列表内(越界 → 末条;NO_SELECTION 不动)。
    // 所有重建路径自愈——修复"删除最后一条后高亮消失"(删除路径此前用删除前的脏
    // FILTERED 长度/历史长度钳制,删末条后选中越界,无行命中高亮)。
    // Clamp the selection into the fresh display list (out of range -> the tail;
    // NO_SELECTION untouched) so every rebuild path self-heals -- fixes the lost highlight
    // after deleting the last row (the delete paths used to clamp against the stale
    // pre-delete FILTERED / history lengths, leaving the selection past the new list).
    {
        let sel = picker_selection();
        set_picker_selection(clamp_selection(sel, filtered.len()));
    }

    // 空态:历史为空 → "暂无历史";有搜索词但无匹配 → "无匹配结果"。共用提示渲染。
    // Empty state: empty history -> "no history"; a query with no matches -> "no match".
    // Both share the same hint rendering.
    let empty_hint = if total == 0 {
        t("clipboard.empty")
    } else if filtered.is_empty() {
        t("clipboard.no_match")
    } else {
        String::new()
    };
    let prepare_ms = prepare_started.elapsed().as_millis();
    let build_rows_started = Instant::now();
    let mut stats = RowCreateStats::default();
    let mut slowest_row_ms = 0;
    let mut slowest_row_index = None;
    if !empty_hint.is_empty() {
        // 容器高度必须取当前 clip view 的实际可视高度,而不是最小窗口高度:筛选后
        // 虽然没有结果,主窗口仍保留原有的较大高度;若用最小值提示会错误地偏到上方。
        // The container height must use the clip view's live visible height, not the minimum
        // window height. Filtering can leave the picker tall with no results; using the
        // minimum would incorrectly place the hint near the top.
        let clip: *mut AnyObject = msg_send![container, superview];
        let visible_h = if clip.is_null() {
            picker_min_height() - header_strip_h() - FOOTER_H
        } else {
            let bounds: NSRect = msg_send![clip, bounds];
            bounds.size.height
        };
        let doc_h = empty_state_doc_height(visible_h);
        let _: () = msg_send![container, setFrameSize: NSSize::new(PICKER_W, doc_h)];
        // 提示文本:在可视列表区内垂直居中。
        // The hint: vertically centered within the visible list area.
        let label_h = 40.0;
        let label_y = (doc_h - label_h) / 2.0;
        let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let label: *mut AnyObject = msg_send![
            label,
            initWithFrame: NSRect::new(
                NSPoint::new(PAD_X, label_y),
                NSSize::new(PICKER_W - PAD_X * 2.0, label_h)
            )
        ];
        // 注意(load-bearing):Apple Silicon 上 TARGET_ABI_USES_IOS_VALUES=1,
        // NSTextAlignment 走 iOS 值分支——Center=1、Right=2(与传统 Mac 相反)。
        // 这里必须用 1 才是居中;传 2 会渲染成右对齐(曾因此"修坏"过)。
        // NOTE (load-bearing): on Apple Silicon TARGET_ABI_USES_IOS_VALUES=1, so
        // NSTextAlignment uses the iOS values -- Center=1, Right=2 (reversed vs classic
        // Mac). 1 is required here for centering; 2 renders right-aligned (a past
        // regression).
        let _: () = msg_send![label, setAlignment: 1isize]; // Center on arm64
        let hint_ns = make_nsstring(&empty_hint);
        let _: () = msg_send![label, setStringValue: hint_ns];
        CFRelease(hint_ns as *const c_void);
        let _: () = msg_send![label, setBezeled: false];
        let _: () = msg_send![label, setDrawsBackground: false];
        let _: () = msg_send![label, setEditable: false];
        // 空态样式按新设计稿 .empty-state:12px、30% 黑。
        // The empty state follows the new mockup's .empty-state: 12px, 30% black.
        let text_color = crate::ffi::hex_to_ns_color(clipboard_palette().muted_text);
        let _: () = msg_send![label, setTextColor: text_color];
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
        let _: () = msg_send![label, setFont: font];
        let _: () = msg_send![container, addSubview: label];
        release_obj(label);
        *EMPTY_STATE_VIEW.lock().unwrap() = Some(ObjPtr::new(label));
        let build_rows_ms = build_rows_started.elapsed().as_millis();
        let finalize_started = Instant::now();
        let rendered_key = picker_rows_key(history_revision(), &query, filter, show_source);
        let finalize_ms = finalize_started.elapsed().as_millis();
        let summary = PickerTimingSummary {
            elapsed_ms: rebuild_started.elapsed().as_millis(),
            history_len: total,
            filtered_len: filtered.len(),
            remove_old_ms,
            prepare_ms,
            build_rows_ms,
            finalize_ms,
            empty: true,
            ..PickerTimingSummary::default()
        };
        with_clipboard_ui(|ui| {
            ui.rendered_rows = Some(rendered_key);
            ui.last_rebuild_timing = Some(summary);
        });
        REBUILDING.store(false, Ordering::SeqCst);
        if summary.elapsed_ms >= CLIPBOARD_SLOW_PATH_MS {
            log_debug!(
                "[clip] picker_rebuild_slow elapsed_ms={} history_len={} filtered_len={} image_rows={} code_rows={} reused=false remove_old_ms={} prepare_ms={} build_rows_ms={} image_ms={} content_attributed_ms={} meta_ms={} finalize_ms={} slowest_row_ms=0 slowest_row_index=none empty=true",
                summary.elapsed_ms,
                summary.history_len,
                summary.filtered_len,
                summary.image_rows,
                summary.code_rows,
                summary.remove_old_ms,
                summary.prepare_ms,
                summary.build_rows_ms,
                summary.image_ms,
                summary.content_attributed_ms,
                summary.meta_ms,
                summary.finalize_ms,
            );
        }
        return Some(summary);
    }

    // 文档高度 = 全部显示条目(滚动区域),由 NSScrollView 滚动。
    // 下限 = 可视区高度(窗口减头部条):文档比可视区矮时悬挂在 clip 底部
    // (clip 不翻转),行会贴底。
    // Document height covers ALL displayed entries (the scrollable area). Floored at the
    // visible height (the window minus the header strip): a document shorter than the
    // visible area hangs off the clip view's bottom (the clip isn't flipped), pushing the
    // rows against the bottom edge.
    let doc_h = (rows_top_offset() + pitches.iter().take(filtered.len()).sum::<f64>() + PAD_Y)
        .max(picker_min_height() - header_strip_h() - FOOTER_H);
    let _: () = msg_send![container, setFrameSize: NSSize::new(PICKER_W, doc_h)];
    let (visible_start, visible_end) = picker_visible_row_range(&pitches, filtered.len());

    let sel_idx = picker_selection();
    // 鼠标悬停行(与选中独立:键盘导航时鼠标停在别的行上 → 两态并存)。
    // The hovered row (independent of the selection: with keyboard navigation the mouse
    // may park on another row -> both states coexist, like the mockup's :hover/.selected).
    // 悬停门禁:指针不在浮窗窗口内时悬停必然不成立(enter/exited 事件可能被
    // REBUILDING 抑制或跨面板穿越吞掉,HOVER_ROW 会冻结成残留值),按 NO_SELECTION
    // 渲染并把静态值归位自愈——否则重建会把幽灵悬停底套到新行上。
    // Hover gate: when the pointer is outside the picker window a hover cannot be valid
    // (enter/exited events may be suppressed by REBUILDING or swallowed across panels,
    // freezing HOVER_ROW into a stale value) -- render without hover and reset the static
    // to self-heal, otherwise rebuilds would paint phantom hover fills onto fresh rows.
    let mut hover_idx = *HOVER_ROW.lock().unwrap();
    if !unsafe { pointer_in_picker_window() } {
        hover_idx = effective_hover_row(false, hover_idx);
        *HOVER_ROW.lock().unwrap() = hover_idx;
    }
    let mut prev_group: Option<DayGroup> = visible_start
        .checked_sub(1)
        .and_then(|index| filtered.get(index))
        .map(|&history_index| day_group(hist[history_index].copied_at));
    let offsets = row_offsets(&pitches);
    for (i, &h_idx) in filtered.iter().enumerate() {
        if i < visible_start || i >= visible_end {
            continue;
        }
        let row_started = Instant::now();
        let y = offsets[i];
        let entry = &hist[h_idx];
        let selected = i == sel_idx;
        let hovered = i == hover_idx;
        let group = day_group(entry.copied_at);
        let has_hdr = prev_group.is_none() || prev_group != Some(group);
        prev_group = Some(group);
        let hdr_h = if has_hdr { GROUP_H } else { 0.0 };
        let row_h = pitches[i] - hdr_h;
        let view = create_row_views(
            container,
            RowSpec {
                entry,
                display_index: i,
                y,
                row_h,
                has_header: has_hdr,
                selected,
                hovered,
                show_source,
                detail_open: detail_visible(),
                sel_idx,
            },
            &mut stats,
        );
        ROW_HOVER_VIEWS.lock().unwrap().push(view);
        ROW_VIEW_INDICES.lock().unwrap().push(i);
        let row_ms = row_started.elapsed().as_millis();
        if row_ms > slowest_row_ms {
            slowest_row_ms = row_ms;
            slowest_row_index = Some(i);
        }
    }

    // 恢复滚动位置 / restore the scroll position.
    if scroll_offset > 0.0 {
        let _: () = msg_send![container, scrollPoint: NSPoint::new(0.0, scroll_offset)];
    }
    let build_rows_ms = build_rows_started.elapsed().as_millis();
    let finalize_started = Instant::now();
    let rendered_key = picker_rows_key(history_revision(), &query, filter, show_source);
    let finalize_ms = finalize_started.elapsed().as_millis();
    let summary = PickerTimingSummary {
        elapsed_ms: rebuild_started.elapsed().as_millis(),
        history_len: total,
        filtered_len: filtered.len(),
        image_rows: stats.image_rows,
        code_rows: stats.code_rows,
        remove_old_ms,
        prepare_ms,
        build_rows_ms,
        image_ms: stats.image_ms,
        content_attributed_ms: stats.content_attributed_ms,
        meta_ms: stats.meta_ms,
        finalize_ms,
        slowest_row_ms,
        slowest_row_index,
        empty: false,
    };
    with_clipboard_ui(|ui| {
        ui.rendered_rows = Some(rendered_key);
        ui.last_rebuild_timing = Some(summary);
    });
    REBUILDING.store(false, Ordering::SeqCst);
    if summary.elapsed_ms >= CLIPBOARD_SLOW_PATH_MS {
        let slowest_row_index = summary
            .slowest_row_index
            .map_or_else(|| "none".to_owned(), |index| index.to_string());
        log_debug!(
            "[clip] picker_rebuild_slow elapsed_ms={} history_len={} filtered_len={} image_rows={} code_rows={} reused=false remove_old_ms={} prepare_ms={} build_rows_ms={} image_ms={} content_attributed_ms={} meta_ms={} finalize_ms={} slowest_row_ms={} slowest_row_index={} empty=false",
            summary.elapsed_ms,
            summary.history_len,
            summary.filtered_len,
            summary.image_rows,
            summary.code_rows,
            summary.remove_old_ms,
            summary.prepare_ms,
            summary.build_rows_ms,
            summary.image_ms,
            summary.content_attributed_ms,
            summary.meta_ms,
            summary.finalize_ms,
            summary.slowest_row_ms,
            slowest_row_index,
        );
    }
    Some(summary)
}

/// 滚动时的增量物化:只补建新进入视口的行、移除离开视口的行,不再整组拆建。
/// 行按显示索引绝对定位(子视图随 NSScrollView 原生滚动),已在视口内的行无需移动;
/// 内容/选中/悬停不变,也无需重建。返回是否有增删。
///
/// Scroll-time incremental materialization: create only the rows newly entering the viewport
/// and remove the ones leaving it, instead of tearing down and rebuilding the whole visible
/// set. Rows are absolutely positioned by display index (subviews scroll natively with the
/// NSScrollView), so surviving rows need no reposition; content/selection/hover are unchanged,
/// so they need no rebuild. Returns whether anything changed.
pub(super) unsafe fn sync_visible_rows() -> bool {
    let Some(container) = picker_container_ptr() else {
        return false;
    };
    let pitches = ROW_PITCHES.lock().unwrap().clone();
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    if pitches.is_empty() || filtered.is_empty() {
        return false;
    }
    let (start, end) = picker_visible_row_range(&pitches, filtered.len());
    {
        // 物化区间未变(升序连续)→ 无需增删。
        // The materialized range is unchanged (ascending contiguous) -> nothing to add/remove.
        let indices = ROW_VIEW_INDICES.lock().unwrap();
        if indices.len() == end.saturating_sub(start)
            && indices.first().copied() == Some(start)
            && indices.last().map(|&last| last + 1) == Some(end)
        {
            return false;
        }
    }
    REBUILDING.store(true, Ordering::SeqCst);
    // 移除离开视口的行。/ Remove rows that left the viewport.
    let stale: Vec<usize> = {
        let indices = ROW_VIEW_INDICES.lock().unwrap();
        indices
            .iter()
            .copied()
            .filter(|&index| index < start || index >= end)
            .collect()
    };
    for index in stale {
        remove_materialized_row(index);
    }
    // 补建新进入视口的行。/ Create rows newly entering the viewport.
    let hist = CLIP_HISTORY.lock().unwrap();
    let offsets = row_offsets(&pitches);
    let sel_idx = picker_selection();
    let hover_idx = *HOVER_ROW.lock().unwrap();
    let show_source = show_source_app();
    let detail_open = detail_visible();
    let mut stats = RowCreateStats::default();
    for i in start..end {
        if row_view_for_display_index(i).is_some() {
            continue;
        }
        let Some(&h_idx) = filtered.get(i) else {
            continue;
        };
        let entry = &hist[h_idx];
        let group = day_group(entry.copied_at);
        // 分组头只依赖显示顺序,与哪些行已物化无关。
        // The group header depends only on display order, not on which rows are materialized.
        let previous_group = if i == 0 {
            None
        } else {
            filtered
                .get(i - 1)
                .and_then(|&previous| hist.get(previous))
                .map(|e| day_group(e.copied_at))
        };
        let has_hdr = previous_group != Some(group);
        let hdr_h = if has_hdr { GROUP_H } else { 0.0 };
        let view = create_row_views(
            container,
            RowSpec {
                entry,
                display_index: i,
                y: offsets[i],
                row_h: pitches[i] - hdr_h,
                has_header: has_hdr,
                selected: i == sel_idx,
                hovered: i == hover_idx,
                show_source,
                detail_open,
                sel_idx,
            },
            &mut stats,
        );
        insert_materialized_row(i, view);
    }
    REBUILDING.store(false, Ordering::SeqCst);
    true
}

/// 删除一行时只移除并重排已有视图;日期分组结构变化时交给完整重建处理。
/// Remove and relayout existing views for a single deletion; fall back to a full rebuild when
/// the date-group structure changes.
unsafe fn try_delete_picker_row_incremental(idx: usize) -> bool {
    let old_views = ROW_HOVER_VIEWS.lock().unwrap().clone();
    let Some(_) = old_views.get(idx) else {
        return false;
    };
    let filter = *CLIP_FILTER.lock().unwrap();
    let query = with_clipboard_ui(|ui| ui.search_query.clone());
    let show_source = show_source_app();
    let hist = CLIP_HISTORY.lock().unwrap();
    let filtered = filtered_indices(&hist, &query, filter);
    // 虚拟列表只物化视口附近的行;删除时让下一次可见刷新重新绑定槽位,避免把物理槽位
    // 错当成完整过滤列表索引。小列表仍可走原有的无闪烁增量路径。
    // A virtualized list materializes only rows near the viewport; let the next visible
    // refresh rebind its slots instead of treating physical slots as full-list indices.
    // Small lists can still use the existing no-flash incremental path.
    let materialized_indices = ROW_VIEW_INDICES.lock().unwrap().clone();
    if materialized_indices != (0..old_views.len()).collect::<Vec<_>>() {
        return false;
    }
    if filtered.len() + 1 != old_views.len() {
        return false;
    }

    let mut previous_group = None;
    for (new_idx, &history_idx) in filtered.iter().enumerate() {
        let group = day_group(hist[history_idx].copied_at);
        let has_header = previous_group.is_none() || previous_group != Some(group);
        previous_group = Some(group);
        let old_idx = if new_idx < idx { new_idx } else { new_idx + 1 };
        if old_views[old_idx].group_label.is_some() != has_header {
            return false;
        }
    }

    REBUILDING.store(true, Ordering::SeqCst);
    let removed = ROW_HOVER_VIEWS.lock().unwrap().remove(idx);
    ROW_VIEW_INDICES.lock().unwrap().remove(idx);
    // bar 是 tile 的子视图,随 tile 一起释放;不能先删 tile 再删 bar(会 use-after-free)。
    // The bar is a tile subview and is released with the tile; never remove the tile and
    // then the bar (use-after-free).
    remove_row_views(&removed);

    let old_hover = *HOVER_ROW.lock().unwrap();
    let new_hover = if old_hover == idx {
        NO_SELECTION
    } else if old_hover > idx && old_hover != NO_SELECTION {
        old_hover - 1
    } else {
        old_hover
    };
    *HOVER_ROW.lock().unwrap() = new_hover;
    with_clipboard_ui(|ui| {
        ui.filtered = filtered.clone();
        ui.picker_selection = clamp_selection(ui.picker_selection, filtered.len());
    });

    let mut pitches = Vec::with_capacity(filtered.len());
    let mut previous_group = None;
    for &history_idx in &filtered {
        let entry = &hist[history_idx];
        let group = day_group(entry.copied_at);
        let header = if previous_group.is_none() || previous_group != Some(group) {
            GROUP_H
        } else {
            0.0
        };
        previous_group = Some(group);
        pitches.push(header + row_content_h(entry));
    }
    *ROW_PITCHES.lock().unwrap() = pitches.clone();
    refresh_footer_count(hist.len());

    let views = ROW_HOVER_VIEWS.lock().unwrap().clone();

    let selection = picker_selection();
    let palette = clipboard_palette();
    let mut previous_group = None;
    let offsets = row_offsets(&pitches);
    for (i, (&history_idx, view)) in filtered.iter().zip(views.iter()).enumerate() {
        let entry = &hist[history_idx];
        let group = day_group(entry.copied_at);
        let has_header = previous_group.is_none() || previous_group != Some(group);
        previous_group = Some(group);
        let y = offsets[i];
        let row_w = PICKER_W - PAD_X * 2.0;
        let header_h = if has_header { GROUP_H } else { 0.0 };
        let content_y = y + header_h;
        let row_h = pitches[i] - header_h;
        if let Some(group_label) = view.group_label {
            let _: () = msg_send![group_label.0, setFrame: NSRect::new(
                NSPoint::new(PAD_X + ROW_PAD_L, y + GROUP_LABEL_PAD),
                NSSize::new(row_w - PAD_X, GROUP_H - GROUP_LABEL_PAD)
            )];
        }
        let _: () = msg_send![view.tile.0, setFrame: NSRect::new(
            NSPoint::new(PAD_X, content_y),
            NSSize::new(row_w, row_h)
        )];
        let _: () = msg_send![view.bar.0, setFrame: NSRect::new(
            NSPoint::new(SEL_BAR_X, SEL_BAR_INSET_Y),
            NSSize::new(SEL_BAR_W, row_h - SEL_BAR_INSET_Y * 2.0)
        )];
        let content_x = PAD_X + ROW_PAD_L;
        let content_w = row_w - ROW_PAD_L - ROW_PAD_R;
        let content_h = row_h - META_FOOTER_H;
        let _: () = msg_send![view.content.0, setFrame: NSRect::new(
            NSPoint::new(content_x, content_y + ROW_PAD_TOP),
            NSSize::new(content_w, content_h - ROW_PAD_TOP - ROW_PAD_BOT)
        )];
        let meta_y = content_y + row_h - META_FOOTER_H - ROW_PAD_BOT;
        let meta_w = row_w - ROW_PAD_L - ROW_PAD_R - ACTIONS_W - 4.0;
        let _: () = msg_send![view.meta.0, setFrame: NSRect::new(
            NSPoint::new(content_x, meta_y),
            NSSize::new(meta_w, META_FOOTER_H)
        )];
        let act_y = meta_y + (META_FOOTER_H - ACTION_H) / 2.0;
        let x_del = PICKER_W - PAD_X - ROW_PAD_R - ACTION_BTN;
        let x_details = x_del - ACTION_GAP - ACTION_BTN;
        let x_pin = x_details - ACTION_GAP - ACTION_BTN;
        for (button, x) in [
            (view.pin, x_pin),
            (view.details, x_details),
            (view.del, x_del),
        ] {
            if !button.0.is_null() {
                let _: () = msg_send![button.0, setFrame: NSRect::new(
                    NSPoint::new(x, act_y),
                    NSSize::new(ACTION_BTN, ACTION_H)
                )];
                let _: () = msg_send![button.0, setTag: i as isize];
            }
        }

        let selected = i == selection;
        let hovered = i == new_hover;
        let background = if selected {
            palette.selection_bg
        } else if hovered {
            palette.hover_bg
        } else {
            0x00000000
        };
        let tile_layer: *mut AnyObject = msg_send![view.tile.0, layer];
        crate::ffi::layer_set_background(tile_layer, crate::ffi::hex_to_cg_color(background));
        let _: () = msg_send![view.bar.0, setHidden: !selected];
        if !entry.pinned {
            let alpha = if selected || hovered { 1.0 } else { 0.0 };
            for button in [view.pin, view.details, view.del] {
                if !button.0.is_null() {
                    let _: () = msg_send![button.0, setAlphaValue: alpha];
                }
            }
        }
        set_detail_action_style(
            view.details.0,
            detail_action_is_active(detail_visible(), selection, i),
            false,
        );
    }

    if let Some(container) = picker_container_ptr() {
        let document_h = (rows_top_offset() + pitches.iter().sum::<f64>() + PAD_Y)
            .max(picker_min_height() - header_strip_h() - FOOTER_H);
        let _: () = msg_send![container, setFrameSize: NSSize::new(PICKER_W, document_h)];
    }
    let rendered_key = picker_rows_key(history_revision(), &query, filter, show_source);
    with_clipboard_ui(|ui| ui.rendered_rows = Some(rendered_key));
    REBUILDING.store(false, Ordering::SeqCst);
    true
}

/// 头部条 flipped:搜索框/清除按钮按顶部坐标布局。
/// The header strip is flipped: the search/clear frames are top-anchored.
extern "C" fn header_strip_is_flipped(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// 容器 flipped:原点在左上,行从顶部排起(最新在最上)。
/// Container is flipped: origin at top-left, rows stack from the top (newest first).
extern "C" fn container_is_flipped(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

/// 按事件坐标解析当前真正位于鼠标下方的可见行。先限制在列表滚动区内,避免头部或
/// 底部坐标转换后误命中滚动文档中的行。
/// Resolve the visible row actually under the event. Gate on the list scroll view first so
/// header/footer coordinates cannot convert into an accidental row hit in the document view.
unsafe fn hover_row_at_event(event: *mut c_void) -> usize {
    if event.is_null() {
        return NO_SELECTION;
    }
    let scroll = match *SCROLL_VIEW.lock().unwrap() {
        Some(scroll) => scroll.0,
        None => return NO_SELECTION,
    };
    let Some(container) = picker_container_ptr() else {
        return NO_SELECTION;
    };
    let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
    let scroll_point: NSPoint = msg_send![
        scroll,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let scroll_bounds: NSRect = msg_send![scroll, bounds];
    if !rect_contains_point(scroll_bounds, scroll_point) {
        return NO_SELECTION;
    }

    let point: NSPoint = msg_send![
        container,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let indices = ROW_VIEW_INDICES.lock().unwrap().clone();
    ROW_HOVER_VIEWS
        .lock()
        .unwrap()
        .iter()
        .position(|row| {
            if row.tile.0.is_null() {
                return false;
            }
            let frame: NSRect = msg_send![row.tile.0, frame];
            rect_contains_point(frame, point)
        })
        .and_then(|slot| indices.get(slot).copied())
        .unwrap_or(NO_SELECTION)
}

/// 浮窗内任意鼠标移动都按整行背景重新核对悬停状态。这样正文按钮退出到行内留白后,
/// 继续移出浮窗也不会因 tracking owner 缺失而留下幽灵样式。
/// Reconcile hover against the whole row backdrop on every picker mouse move. This prevents
/// a stale style when the pointer leaves a content button through row padding and then exits.
extern "C" fn container_mouse_moved(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    if REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    let row = unsafe { hover_row_at_event(event) };
    set_hover_row(row);
}

/// 鼠标离开整个浮窗时兜底清空;此事件由固定父视图的 InVisibleRect tracking area
/// 提供,不依赖任何行内按钮是否收到 mouseExited。
/// Clear hover when leaving the whole picker. The fixed parent's InVisibleRect tracking area
/// supplies this event independently of whether any row button receives mouseExited.
extern "C" fn container_mouse_exited(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    if !REBUILDING.load(Ordering::SeqCst) {
        set_hover_row(NO_SELECTION);
    }
}

/// 行按钮类(NSButton 子类,重写 mouseEntered: 实现悬停选中)。
/// Row-button class (NSButton subclass; mouseEntered: implements hover selection).
unsafe fn row_button_class() -> *mut AnyObject {
    static ROW_BTN_CLS: OnceLock<StaticClass> = OnceLock::new();
    ROW_BTN_CLS
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardRowButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                row_button_mouse_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                row_button_mouse_exited as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

fn update_hover_visuals(prev: usize, new: usize) {
    let sel = picker_selection();
    let hist = CLIP_HISTORY.lock().unwrap();
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    // 与建行时的样式常量保持一致(选中 0.050 优先于悬停 0.032)。
    // Keep in sync with the constants at row creation (selected 0.050 beats hovered 0.032).
    const SEL_BG: f64 = 0.050;
    const HOVER_BG: f64 = 0.032;
    unsafe {
        for i in [prev, new] {
            if i == NO_SELECTION {
                continue;
            }
            let Some(rv) = row_view_for_display_index(i) else {
                continue;
            };
            let selected = i == sel;
            let hovered = i == new;
            let bg_alpha = if selected {
                SEL_BG
            } else if hovered {
                HOVER_BG
            } else {
                0.0
            };
            let layer: *mut AnyObject = msg_send![rv.tile.0, layer];
            let bg: *mut AnyObject =
                msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: bg_alpha];
            crate::ffi::layer_set_background(layer, crate::ffi::ns_color_to_cg(bg));
            if !rv.bar.0.is_null() {
                let _: () = msg_send![rv.bar.0, setHidden: !selected];
            }
            // 按钮透明度:置顶条目常显(恒 1.0);非置顶条目 = 悬停/选中才显现。
            // Button alpha: pinned entries keep them always visible (fixed 1.0); unpinned
            // entries show them on hover/selection only.
            let pinned = filtered
                .get(i)
                .and_then(|&h| hist.get(h))
                .map(|e| e.pinned)
                .unwrap_or(false);
            if !pinned {
                let act_alpha: f64 = if selected || hovered { 1.0 } else { 0.0 };
                for b in [rv.pin, rv.details, rv.del] {
                    if !b.0.is_null() {
                        let _: () = msg_send![b.0, setAlphaValue: act_alpha];
                    }
                }
            }
        }
    }
}

fn set_hover_row(new: usize) {
    let mut hover = HOVER_ROW.lock().unwrap();
    let prev = *hover;
    if prev == new {
        return;
    }
    *hover = new;
    drop(hover);
    update_hover_visuals(prev, new);
}

/// 搜索框聚焦时列表没有键盘选中项,但过滤后的条目仍应显示独立的鼠标悬停样式。
/// With search focus, the list has no keyboard-selected row, but filtered entries must still
/// show their independent mouse-hover style.
extern "C" fn row_button_mouse_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    // 重建期间派发的 enter 忽略(防无限递归,见 REBUILDING 注释)。
    // Ignore enters dispatched during a rebuild (prevents infinite recursion; see REBUILDING).
    if REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    let idx: isize = unsafe { msg_send![_self as *mut AnyObject, tag] };
    if idx >= 0 {
        // 悬停只更新 hover 行(轻底,0.032),**不改选中**——选中(0.050 + 左条)只由
        // 键盘方向键/点击驱动。两个状态因此能同时可见,对应设计稿里独立的
        // .item:hover 与 .item.selected(悬停即选中会让悬停行恒为选中样式,
        // 轻悬停底无法显示,两种状态看着就一样)。
        // Hovering only sets the hovered row (the light 0.032 fill) and does NOT move the
        // selection (0.050 + the left bar) -- the selection moves via the keyboard arrows
        // / clicks only. The two states stay independently visible, matching the mockup's
        // separate .item:hover and .item.selected rules (auto-select-on-hover would always
        // render the hovered row as the selected style, making them look identical).
        // 增量刷新悬停视觉,不重建。
        // Incremental hover visuals, no rebuild.
        set_hover_row(idx as usize);
    }
}

/// 判断鼠标是否仍在整行区域内,包括右侧独立的操作按钮。
/// Check whether the pointer is still inside the whole row, including its separate action buttons.
pub(super) unsafe fn mouse_inside_row(event: *mut c_void, idx: usize) -> bool {
    let Some(row) = row_view_for_display_index(idx) else {
        return false;
    };
    if row.tile.0.is_null() {
        return false;
    }
    let Some(container) = picker_container_ptr() else {
        return false;
    };
    // locationInWindow 使用窗口基准坐标;fromView 必须是 NSView,不能误传 NSWindow。
    // locationInWindow uses the window-base coordinate system; fromView must be an NSView,
    // never an NSWindow.
    let location: NSPoint = msg_send![event as *mut AnyObject, locationInWindow];
    let point: NSPoint = msg_send![
        container,
        convertPoint: location,
        fromView: std::ptr::null::<AnyObject>()
    ];
    let frame: NSRect = msg_send![row.tile.0, frame];
    rect_contains_point(frame, point)
}

/// 清除指定行的悬停状态,并同步收起该行的 hover 视觉。
/// Clear a row's hover state and synchronously collapse its hover visuals.
pub(super) fn clear_hover_row_if(idx: usize) {
    let hovered = *HOVER_ROW.lock().unwrap();
    if hovered == idx {
        set_hover_row(NO_SELECTION);
    }
}

/// 鼠标离开行按钮:仅在真正离开整行时清除悬停,避免移向右下角操作按钮时消失。
/// Mouse leaves a row button: clear hover only after leaving the whole row, so moving to the
/// bottom-right action buttons does not hide them mid-transition.
extern "C" fn row_button_mouse_exited(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    if REBUILDING.load(Ordering::SeqCst) {
        return;
    }
    let idx: isize = unsafe { msg_send![_self as *mut AnyObject, tag] };
    if idx >= 0 {
        if unsafe { mouse_inside_row(event, idx as usize) } {
            return;
        }
        clear_hover_row_if(idx as usize);
    }
}

/// 行点击(按钮 tag = 行索引)→ 粘贴该行;Option+点击(设置开启)= 粘贴并删除。
/// 修饰键从 currentEvent 读:action 在 mouseUp 时触发,currentEvent 就是这一次点击
/// (与 keyboard 路径的 Option+Enter 等价;开关关闭时 Option 被忽略,普通粘贴)。
/// Row click (button tag = row index) -> paste that row; Option+click (when the setting
/// is on) = paste and delete. The modifiers come from currentEvent: the action fires on
/// mouseUp, so currentEvent IS this click (equivalent to Option+Enter on the keyboard
/// path; with the toggle off Option is ignored and the normal paste runs).
pub(super) extern "C" fn handle_clipboard_row_click(
    _self: *mut c_void,
    _cmd: Sel,
    sender: *mut c_void,
) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx >= 0 {
        // NSEventModifierFlagOption = 1 << 19(Alternate)= 0x08_0000。currentEvent 是
        // NSApplication 的**实例**方法,必须先取 sharedApplication——直接发给 Class
        // 对象会抛 unrecognized selector,异常炸穿 AppKit 事件循环,把浮窗卡死。
        // NSEventModifierFlagOption (Alternate) = 1 << 19 = 0x08_0000. currentEvent is an
        // INSTANCE method on NSApplication: go through sharedApplication first. Sending it
        // to the Class object raises an unrecognized-selector exception that unwinds
        // through AppKit's event loop and wedges the picker.
        let option_held = unsafe {
            let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let ev: *mut AnyObject = if nsapp.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![nsapp, currentEvent]
            };
            if ev.is_null() {
                false
            } else {
                let flags: u64 = msg_send![ev, modifierFlags];
                (flags & 0x0008_0000) != 0
            }
        };
        if option_held {
            paste_at_ex(idx as usize, true);
        } else {
            paste_at(idx as usize);
        }
    }
}

/// 图钉按钮回调(tag = 显示行索引)→ 映射历史索引置顶/取消置顶并刷新列表。
/// Pin-button callback (tag = display row index) -> mapped history index, pin/unpin, refresh.
pub(super) extern "C" fn toggle_pin(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx < 0 {
        return;
    }
    let Some(h_idx) = mapped_index(idx as usize) else {
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    let (now_pinned, new_h_idx) = toggle_pin_on(&mut hist, h_idx);
    drop(hist);
    save_history();
    unsafe { rebuild_rows() };
    // 跟随置顶设置:选中被操作条目(用重排后的新索引)→ 再重建一次刷新高亮。
    // Follow-pin setting: select the toggled entry (the POST-REORDER index), then rebuild
    // once more.
    if selection_after_pin(new_h_idx) {
        unsafe { rebuild_rows() };
    }
    // 置顶会改变条目在列表中的位置;详情面板按选中行重新定位并刷新内容。
    // Pinning changes the row position; reposition and refresh the detail panel from the
    // current selection so it follows the reordered row.
    if detail_visible() {
        unsafe { show_detail_for_sel() };
    }
    let msg = if now_pinned {
        t("clipboard.toast_pinned")
    } else {
        t("clipboard.toast_unpinned")
    };
    show_toast(&msg);
}

/// 置顶/取消置顶后按设置移动选中(`clipboard.pin_follow_selection`):
/// - 跟随置顶(true,默认):选中移到被操作条目的**新显示位置**(置顶 → 列表顶,
///   取消置顶 → 非置顶区顶部);
/// - 保持当前位置(false):不动(rebuild_rows 只做越界钳制,选中指向原下一条,
///   便于批量置顶)。
///
/// 返回是否移动了选中(调用方据此再补一次 rebuild_rows 刷新高亮)。`new_h_idx` 是
/// 条目**重排后**的历史索引(toggle_pin_on 返回的新索引)——旧索引此时已指向别的
/// 条目,搜新列表会落在旧位置,等于"保持当前位置"(曾因此"跟随"不生效)。
/// Move the selection after pin/unpin per `clipboard.pin_follow_selection`:
/// - Follow (true, default): select the toggled entry's NEW display position (pin -> the
///   top of the list; unpin -> the top of the unpinned block);
/// - Keep (false): leave it (rebuild_rows only clamps; the selection points at the next
///   entry, convenient for batch pinning).
///
/// Returns whether the selection moved (the caller then rebuilds once more to refresh the
/// highlight). `new_h_idx` is the entry's POST-REORDER history index (returned by
/// toggle_pin_on) -- the OLD index already refers to a different entry, so searching the
/// fresh list with it would land on the old position, i.e. exactly "keep current" (the
/// follow mode once failed this way).
fn selection_after_pin(new_h_idx: usize) -> bool {
    if !CONFIG.read().unwrap().clipboard.pin_follow_selection {
        return false;
    }
    let filtered = with_clipboard_ui(|ui| ui.filtered.clone());
    if let Some(pos) = filtered.iter().position(|&h| h == new_h_idx) {
        set_picker_selection(pos);
        true
    } else {
        false
    }
}

/// 删除按钮回调(tag = 显示行索引)→ 映射历史索引删除并刷新列表。
/// Delete-button callback (tag = display row index) -> mapped history index, remove, refresh.
/// 详情按钮回调(tag = 显示行索引)→ 选中该行并打开详情面板(与 → 键同路径)。
/// **toggle**:详情已开且点的正是当前选中行 → 关闭(再点一下取消详情);否则照常
/// 选中该行并打开/刷新详情(跨行点击时详情跟随新行)。
/// The details-button callback (tag = display row index) -> select the row and open the
/// detail panel (the same path as the → key). TOGGLE: with the detail already open and
/// the click landing on the CURRENTLY selected row, close it (a second click cancels the
/// detail); otherwise select the row and open/refresh the detail (a different row's click
/// moves the detail along to the new row).
pub(super) extern "C" fn show_item_details_cb(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx < 0 {
        return;
    }
    let previous = picker_selection();
    // 已打开且点的是当前选中行 → 本次点击是"取消详情"。
    // Detail already open AND the click is on the selected row -> this click cancels it.
    let close = detail_visible() && previous == idx as usize;
    set_picker_selection(idx as usize);
    unsafe {
        // 详情按钮点击只需要更新前后两行的视觉状态,无需同步重建整个剪贴板列表。
        // A detail-button click only needs the incremental visual update for the old and new
        // selection; rebuilding the entire clipboard list here made mouse opening feel slow.
        refresh_selection(previous, idx as usize);
        if close {
            hide_detail();
        } else {
            show_detail_for_sel();
        }
    }
}

pub(super) extern "C" fn delete_entry_cb(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let idx: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    if idx < 0 {
        return;
    }
    let Some(h_idx) = mapped_index(idx as usize) else {
        return;
    };
    let mut hist = CLIP_HISTORY.lock().unwrap();
    let Some(removed_entry) = remove_entry_for_undo(&mut hist, h_idx) else {
        return;
    };
    // 被删行在选中行上方 → 选中下移一格(保持指向同一条);被删行即选中行或在其下方
    // → 不动(前者指向原下一条)。无选中哨兵(搜索框聚焦)不动。越界钳制统一交给
    // rebuild_rows 在 FILTERED 重算后处理——此前用 hist.len() 钳制显示索引:无搜索词
    // 时两者恰好相等才碰巧正确,搜索过滤时维度不匹配,删末条后仍会越界、高亮消失。
    // A deleted row ABOVE the selection shifts it down one (the same entry stays selected);
    // deleting the selected row or a row below leaves it alone (the former points at the
    // next entry). The no-selection sentinel (search-field focus) is untouched. The
    // out-of-range clamp happens in rebuild_rows after FILTERED is recomputed -- this used
    // to clamp the display index against hist.len(): correct only by coincidence without a
    // search query, dimensionally wrong under a filter, and still past the list after
    // deleting the tail (the lost highlight).
    let previous = picker_selection();
    let deleted_selected = previous != NO_SELECTION && previous == idx as usize;
    if previous != NO_SELECTION && (idx as usize) < previous {
        set_picker_selection(previous - 1);
    }
    drop(hist);
    remember_deleted_clipboard_entry(removed_entry, h_idx);
    let incremental = unsafe { try_delete_picker_row_incremental(idx as usize) };
    save_history();
    if !incremental {
        schedule_picker_refresh();
    }
    // 详情面板跟随选中条目;若删的正是选中条目则关闭它(避免残留已删内容)。
    // The detail panel follows the selected entry; when the deleted row WAS the selection,
    // close the panel (no stale content).
    if deleted_selected && detail_visible() {
        hide_detail();
    }
}

/// 粘贴指定显示索引的条目(经 FILTERED 映射):关闭浮窗 + 写回剪贴板 + 模拟 Cmd+V。
/// Paste the entry at display `idx` (mapped through FILTERED): close the picker + write back
/// to the pasteboard + synthesize Cmd+V.
fn paste_at(idx: usize) {
    paste_at_ex(idx, false);
}

/// paste_at 的焚后变体:粘贴成功后立即从历史中删除该条目(Option+回车/点击,
/// 一次性粘贴)。删除走 delete_entry(图片缓存文件一并清理),置顶条目同样可焚。
/// The burn-after-paste variant of paste_at: on a successful paste the entry is removed
/// from the history right away (Option+Enter/click, one-shot paste). Removal goes through
/// delete_entry (the image cache file goes too); pinned entries burn all the same.
fn paste_at_ex(idx: usize, delete_after: bool) {
    let Some(h_idx) = mapped_index(idx) else {
        log_debug!("[clip] paste index {} out of range", idx);
        hide_picker();
        return;
    };
    let entry = {
        let hist = CLIP_HISTORY.lock().unwrap();
        hist.get(h_idx).cloned()
    };
    let Some(entry) = entry else {
        log_debug!("[clip] paste index {} out of range", idx);
        hide_picker();
        return;
    };
    // 实际生效 = 手势 ∧ 设置:开关关闭时 Option 只是普通粘贴(修饰键被忽略)。
    // Effective = gesture AND setting: with the toggle off, Option falls back to a plain
    // paste (the modifier is ignored).
    let burn = delete_after && delete_after_paste();
    if burn {
        // 布防必须在写回之前(同步通知重入场景,见布防函数注释)。
        // Arm BEFORE the write-back (the synchronous-notification re-entry case; see the
        // arming function's comment).
        arm_paste_delete_suppression();
    }
    hide_picker();
    unsafe {
        if let Some(img) = &entry.image {
            // 文件复制条目:源文件还在 → 恢复文件语义(file-url)粘贴(应用按需读
            // 原文件);源文件已删除/移动 → 直接跳过(文件条目不存字节,无内容可回退)。
            // A file-copy entry: if the source file still exists, restore file semantics
            // (file-url) -- the target app reads the original file on demand; if the source
            // is deleted/moved, skip the paste (a file entry stores no bytes to fall back
            // to).
            let ok = match paste_kind(img) {
                PasteKind::File(path) => write_pasteboard_file(&path),
                PasteKind::Image if img.source_path.is_some() => {
                    log_info!("[clip] paste skipped: source file gone (uti={})", img.uti);
                    false
                }
                PasteKind::Image => write_pasteboard_image(img),
            };
            // 写回失败(如缓存缺失)时跳过合成 Cmd+V,避免把旧剪贴板内容粘出去。
            // On a failed write-back (e.g. cache miss) skip the synthesized Cmd+V, so the
            // OLD pasteboard content is not pasted.
            if ok {
                let pasted = synthesize_paste();
                if burn && pasted {
                    delete_burned_entry(&entry);
                    if clear_system_pasteboard_after_paste() {
                        schedule_system_pasteboard_clear();
                    }
                } else if burn {
                    log_info!("[clip] paste-and-delete skipped: Cmd+V event creation failed");
                    disarm_paste_delete_suppression();
                }
            } else {
                // 写回失败 = 没发生写回,撤防抑制,条目保留(不能"没粘上还丢了记录")。
                // A failed write-back means nothing was written: disarm so the suppression
                // cannot swallow the next genuine copy, and keep the entry (never lose the
                // record without a paste).
                if burn {
                    disarm_paste_delete_suppression();
                }
            }
        } else {
            // 粘贴回写:打 marker(轮询跳过,防止粘贴被当成新复制移动条目)。
            // Paste write-back: stamp the marker (the poll skips it, so a paste is never
            // re-captured as a fresh copy that reorders the history).
            let ok = write_pasteboard_text(&entry.text, true);
            if ok {
                let pasted = synthesize_paste();
                if burn && pasted {
                    delete_burned_entry(&entry);
                    if clear_system_pasteboard_after_paste() {
                        schedule_system_pasteboard_clear();
                    }
                } else if burn {
                    log_info!("[clip] paste-and-delete skipped: Cmd+V event creation failed");
                    disarm_paste_delete_suppression();
                }
            } else if burn {
                log_info!("[clip] paste-and-delete skipped: text write-back failed");
                disarm_paste_delete_suppression();
            }
        }
    }
}

/// 焚后粘贴的删除步骤:条目移出历史(图片缓存文件一并删除)并落盘;日志只记类型
/// 与计数,不记条目内容。
/// The removal step of burn-after-paste: drop the entry from the history (the image cache
/// file goes too) and persist; logs record only the kind and count, never the content.
fn delete_burned_entry(target: &ClipEntry) {
    let kind = {
        let mut hist = CLIP_HISTORY.lock().unwrap();
        let Some(h_idx) = hist
            .iter()
            .position(|entry| same_clip_entry_identity(entry, target))
        else {
            log_debug!("[clip] paste-and-delete: entry already gone");
            return;
        };
        let kind = match hist.get(h_idx) {
            Some(e) if e.image.is_some() => "image",
            Some(_) => "text",
            None => "gone",
        };
        if kind != "gone" {
            delete_entry(&mut hist, h_idx);
        }
        kind
    };
    save_history();
    log_info!(
        "[clip] pasted and deleted (burn after paste, kind={})",
        kind
    );
}

/// 粘贴内容判定:文件复制条目且源文件仍存在 → 文件粘贴(恢复 file-url);
/// 其余情况 → 图片数据粘贴(按原始 UTI)。纯函数,便于单测。
/// Decide the paste kind: a file-copy entry whose source file still exists pastes as a
/// FILE (restoring the file-url); everything else pastes as image data (original UTI).
/// Pure, unit-tested.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum PasteKind {
    File(String),
    Image,
}

pub(super) fn paste_kind(img: &ImageEntry) -> PasteKind {
    match &img.source_path {
        Some(path) if std::path::Path::new(path).exists() => PasteKind::File(path.clone()),
        _ => PasteKind::Image,
    }
}

/// 先关闭浮窗后合成 Cmd+V(keyDown + keyUp,post 到 session 层)。
/// 浮窗是 key window(NonactivatingPanel + makeKeyWindow),此时合成键盘事件会被路由给
/// 浮窗所属的 app(我们自己),输入框收不到;orderOut 后面板失去 key,系统 key window
/// 回归原应用,合成事件才能到达用户原来的输入框。
/// Synthesize Cmd+V (keyDown + keyUp, posted at the session level) AFTER the picker is
/// closed. The panel is the key window (NonactivatingPanel + makeKeyWindow), so a
/// synthesized key event would be routed to the panel's app (us) and never reach the input
/// field; once ordered out, the panel resigns key, the system key window returns to the
/// previous app, and the synthesized Cmd+V lands in the user's input field.
unsafe fn synthesize_paste() -> bool {
    let down = CGEventCreateKeyboardEvent(std::ptr::null(), VK_V, true);
    let Some(down) = (!down.is_null()).then_some(down) else {
        return false;
    };
    CGEventSetFlags(down, K_CG_EVENT_FLAG_MASK_COMMAND);
    CGEventPost(K_CG_SESSION_EVENT_TAP, down);
    let up = CGEventCreateKeyboardEvent(std::ptr::null(), VK_V, false);
    if up.is_null() {
        // 创建失败也必须释放已 post 的 down:CGEventCreate* 返回 +1,post 不接管所有权。
        // Release the already-posted `down` on this failure path too: CGEventCreate* returns
        // +1 and posting does not take ownership.
        CFRelease(down as *const c_void);
        return false;
    }
    CGEventSetFlags(up, K_CG_EVENT_FLAG_MASK_COMMAND);
    CGEventPost(K_CG_SESSION_EVENT_TAP, up);
    CFRelease(down as *const c_void);
    CFRelease(up as *const c_void);
    true
}

/// 方向键导航纯逻辑:↑(126)/↓(125) 返回新的选中索引(循环);其它键返回 None。
/// Pure arrow-key navigation: up (126) / down (125) return the next selection (wrapping);
/// any other key returns None.
pub(super) fn nav_arrow(keycode: u16, sel: usize, hist_len: usize) -> Option<usize> {
    if hist_len == 0 {
        return None;
    }
    // sel 可能为 NO_SELECTION(usize::MAX,焦点在搜索框时的哨兵)——冒烟直接驱动
    // handler 会走到这里,必须防溢出并视为"无选中"处理。
    // sel may be NO_SELECTION (usize::MAX, the sentinel while the search field has focus) --
    // the smoke drives the handler directly so this must not overflow and treats it as
    // "no selection".
    match keycode {
        126 => Some(if sel == 0 || sel >= hist_len {
            hist_len - 1
        } else {
            sel - 1
        }),
        125 => Some(if sel >= hist_len - 1 { 0 } else { sel + 1 }),
        _ => None,
    }
}

/// 删除/裁剪后把选中索引钳制到当前显示列表内(越界 → 末条);
/// 无选中哨兵(NO_SELECTION)不动。纯函数,供 rebuild_rows 与删除路径共用,单测覆盖。
/// Clamp a selection index into the current display list after deletions/trims
/// (out of range -> the tail); the no-selection sentinel (NO_SELECTION) is left untouched.
/// Pure function shared by rebuild_rows and the delete paths; unit-tested.
pub(super) fn clamp_selection(sel: usize, len: usize) -> usize {
    if sel == NO_SELECTION || len == 0 {
        return sel;
    }
    sel.min(len - 1)
}

/// 键盘导航:Tab 循环分类,↑/↓ 选择,← 置顶,→ 展开详情(详情打开时 → 关闭详情),
/// Enter 粘贴,Esc 关闭。
/// Keyboard navigation: Tab cycles filters; up/down select, left pins, right expands
/// details (with the detail open, right closes it), Enter pastes, Esc closes.
/// C 回调的 panic 边界:panic 穿不过 extern "C" 帧(会 abort 整个进程),这里统一接住。
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
pub(super) extern "C" fn container_key_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    crate::callback_guard::void("container_key_down", || unsafe {
        container_key_down_inner(_self, _cmd, event)
    });
}

unsafe fn container_key_down_inner(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let keycode: u16 = msg_send![event as *mut AnyObject, keyCode];
        // Cmd+F(键码 3 + Command 修饰 0x100000):聚焦顶部搜索框。搜索框已聚焦时
        // 按键由字段编辑器消化,不会到达这里——天然无操作。
        // Cmd+F (keycode 3 + Command modifier 0x100000): focus the top search field.
        // When the field is already focused, the key goes to the field editor and never
        // reaches here -- a natural no-op.
        let mods: u64 = msg_send![event as *mut AnyObject, modifierFlags];
        // Cmd+Z(键码 6)只由列表容器处理;搜索框的 field editor 会先接收它,保留文本撤销。
        // Cmd+Z (keycode 6) is handled only by the list container; the search field's field
        // editor receives it first, preserving native text-editing undo.
        if is_clipboard_undo_shortcut(keycode, mods) {
            if undo_deleted_clipboard_entry().is_some() {
                rebuild_rows();
                show_toast(&t("clipboard.toast_undo_delete"));
            }
            return;
        }
        // Cmd+C(键码 8):详情打开时复制选中范围(无选中 = 复制全文)。键盘路径与
        // 详情底部的"复制所选"按钮等价——详情面板不会成为 key,系统 Cmd+C 路由
        // 到主浮窗,这里手动转发。搜索框聚焦时按键由字段编辑器消化,天然不冲突。
        // Cmd+C (keycode 8): with the detail open, copy the selection (full text when
        // nothing is selected) -- the keyboard twin of the detail's "copy selection"
        // button. The detail never becomes key, so the system routes Cmd+C to the picker;
        // we forward it here. With the search field focused the key goes to the field
        // editor first, so no conflict.
        if keycode == 8 && (mods & 0x0010_0000) != 0 && detail_visible() {
            copy_detail_selection();
            return;
        }
        if keycode == 3 && (mods & 0x0010_0000) != 0 {
            if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
                let window = match *PICKER_WINDOW.lock().unwrap() {
                    Some(w) => w.0,
                    None => return,
                };
                // makeFirstResponder: 返回 BOOL('B')。
                // makeFirstResponder: returns BOOL ('B').
                let _: bool = msg_send![window, makeFirstResponder: f.0];
            }
            return;
        }
        // 可选中范围是当前显示列表(搜索过滤后;超出可视部分靠滚动查看)。
        // The selectable range is the current display list (post-filter; scrolling reveals
        // the rest).
        let display_len = with_clipboard_ui(|ui| ui.filtered.len());
        let sel = picker_selection();
        match keycode {
            48 => {
                // Tab(48):按固定顺序循环筛选分类;详情若已展开则先关闭,避免筛选后
                // 详情遗留一条不属于当前列表的陈旧内容。
                // Tab(48): cycle filters in the fixed order. Close an open detail first so
                // filtering cannot leave stale content that no longer belongs to the list.
                let next = {
                    let active = CLIP_FILTER.lock().unwrap();
                    next_clip_filter(*active)
                };
                apply_clip_filter(next);
            }
            123 => {
                // ←(123):无论详情是否打开,都切换当前选中条目的置顶状态。
                // Left: toggle the selected entry's pinned state whether or not the detail
                // panel is open.
                let idx = sel;
                let Some(h_idx) = mapped_index(idx) else {
                    return;
                };
                let mut hist = CLIP_HISTORY.lock().unwrap();
                let (now_pinned, new_h_idx) = toggle_pin_on(&mut hist, h_idx);
                drop(hist);
                save_history();
                rebuild_rows();
                // 跟随置顶设置:选中被操作条目(用重排后的新索引)→ 再重建一次刷新高亮。
                // Follow-pin setting: select the toggled entry (the POST-REORDER index),
                // then rebuild once more to refresh the highlight.
                if selection_after_pin(new_h_idx) {
                    rebuild_rows();
                }
                // 置顶会改变条目在列表中的位置;详情保持打开并跟随新的选中行。
                // Pinning changes the row position; keep the detail open and follow the new
                // selected row.
                if detail_visible() {
                    show_detail_for_sel();
                }
                let msg = if now_pinned {
                    t("clipboard.toast_pinned")
                } else {
                    t("clipboard.toast_unpinned")
                };
                show_toast(&msg);
            }
            124 => {
                // →(124):详情打开时关闭详情(与 ← 一致);否则展开选中条目的详情
                // (完整文本 / 图片大图)。
                // Right: closes the detail panel when it is open (same as ←); otherwise
                // expands the selected entry's details (full text / large image).
                if detail_visible() {
                    hide_detail();
                    return;
                }
                let idx = sel;
                if idx == NO_SELECTION {
                    return;
                }
                show_detail_for_sel();
            }
            126 | 125 => {
                // ↑(126):已在列表第一条(或无选中)时跳回搜索框;进入前清除选中,
                // 高光消失(delegate 的 controlTextDidBeginEditing: 也会清,双保险)。
                // Up (126): at the first list entry (or no selection), jump focus back to the
                // search field; clear the selection BEFORE entering so the highlight goes away
                // (the controlTextDidBeginEditing: delegate also clears - belt and braces).
                if keycode == 126 && (sel == 0 || sel == NO_SELECTION) {
                    if let Some(f) = *SEARCH_FIELD.lock().unwrap() {
                        // 只清除原选中行的高光(增量),不重建整表。
                        // Clear only the previously selected row's highlight (incremental);
                        // do not rebuild the whole list.
                        set_picker_selection(NO_SELECTION);
                        refresh_selection(sel, NO_SELECTION);
                        if detail_visible() {
                            refresh_detail_action_visuals();
                        }
                        let window = match *PICKER_WINDOW.lock().unwrap() {
                            Some(w) => w.0,
                            None => return,
                        };
                        // makeFirstResponder: 返回 BOOL('B')。
                        // makeFirstResponder: returns BOOL ('B').
                        let _: bool = msg_send![window, makeFirstResponder: f.0];
                        return;
                    }
                }
                let previous = sel;
                let idx = if let Some(next) = nav_arrow(keycode, sel, display_len) {
                    set_picker_selection(next);
                    next
                } else {
                    sel
                };
                refresh_selection(previous, idx);
                // 滚动到选中行可见 / scroll the selection into view.
                if let Some(container) = picker_container_ptr() {
                    scroll_selection_into_view(container, idx);
                }
                // 详情打开时跟随选中条目实时刷新(浏览体验,类似 Quick Look)。
                // The detail panel follows the selection live while open (Quick-Look-style
                // browsing).
                if detail_visible() {
                    show_detail_for_sel();
                }
            }
            36 => {
                // Enter;Option+Enter(设置开启)= 粘贴并删除(一次性粘贴)。
                // 修饰掩码沿用本函数顶部的 NSEventModifierFlags 位:Command=0x10_0000,
                // Option(Alternate)=0x08_0000。
                // Enter; Option+Enter (when the setting is on) = paste and delete (one-shot
                // paste). Modifier bits follow NSEventModifierFlags as at the top of this
                // function: Command=0x10_0000, Option (Alternate)=0x08_0000.
                let idx = sel;
                if (mods & 0x0008_0000) != 0 {
                    paste_at_ex(idx, true);
                } else {
                    paste_at(idx);
                }
            }
            51 => {
                // Backspace(删除键):删除选中条目并刷新。
                // Backspace (delete): remove the selected entry and refresh.
                let idx = sel;
                let Some(h_idx) = mapped_index(idx) else {
                    return;
                };
                let mut hist = CLIP_HISTORY.lock().unwrap();
                let Some(removed_entry) = remove_entry_for_undo(&mut hist, h_idx) else {
                    return;
                };
                // 删除的是选中行本身 → 选中保持原位(指向原下一条);删末条后越界则由
                // rebuild_rows 在 FILTERED 重算后钳到新末条——此前用删除前的脏
                // FILTERED 长度钳制,删末条后选中越界、无行命中高亮,高亮消失。
                // Deleting the selected row keeps the selection in place (pointing at the
                // next entry); an out-of-range selection (deleted the tail) is clamped to
                // the new tail by rebuild_rows after FILTERED is recomputed -- the old code
                // clamped against the stale pre-delete FILTERED length, so the selection
                // stayed past the new list, no row matched, and the highlight vanished.
                drop(hist);
                remember_deleted_clipboard_entry(removed_entry, h_idx);
                let incremental = try_delete_picker_row_incremental(idx);
                save_history();
                if !incremental {
                    schedule_picker_refresh();
                }
                // 详情面板跟随选中条目,而选中条目刚被删除 → 关闭,避免残留已删内容。
                // The detail panel follows the selected entry, which was just deleted ->
                // close it, so no stale content lingers.
                hide_detail();
            }
            53 => {
                if clear_history_confirmation_expanded() {
                    set_clear_history_confirmation_expanded(false);
                    return;
                }
                // Esc:详情打开时第一级 = 关闭详情(浮窗与搜索词保持不动)。
                // Esc: with the detail open, the first press closes the detail (the picker
                // and the query stay untouched).
                if detail_visible() {
                    hide_detail();
                    return;
                }
                // Esc:清空搜索词则恢复全列表,再按才关闭——搜索框聚焦时的第一级由
                // NSSearchField 子类的 cancelOperation: 处理;这里处理列表聚焦时。
                // Esc: a query gets cleared first (restoring the full list), a second press
                // closes. The search-field-focused first level is handled by the
                // NSSearchField subclass's cancelOperation:; this handles list focus.
                let had_query = with_clipboard_ui(|ui| {
                    let had_query = !ui.search_query.is_empty();
                    ui.search_query.clear();
                    had_query
                });
                if had_query {
                    rebuild_rows();
                } else {
                    hide_picker();
                }
            }
            _ => {}
        }
    }
}

/// 根据可视区与选中行位置计算滚动偏移,避免快速按键时 scrollRectToVisible 与重建互相覆盖。
/// Compute the scroll offset from the viewport and selected-row geometry, avoiding the race-like
/// interaction between scrollRectToVisible and rapid row rebuilds.
pub(super) fn selection_scroll_offset(
    current: f64,
    viewport_h: f64,
    document_h: f64,
    y: f64,
    h: f64,
) -> f64 {
    let max_offset = (document_h - viewport_h).max(0.0);
    let target = if y < current {
        y
    } else if y + h > current + viewport_h {
        y + h - viewport_h
    } else {
        current
    };
    target.max(0.0).min(max_offset)
}

/// 直接把选中行滚入可视区,不依赖 AppKit 的异步可见性调整。
/// Scroll the selected row into view directly, without relying on AppKit's asynchronous
/// visibility adjustment.
pub(super) unsafe fn scroll_selection_into_view(container: *mut AnyObject, idx: usize) {
    let (y, h) = {
        let pitches = ROW_PITCHES.lock().unwrap();
        let Some(&h) = pitches.get(idx) else {
            return;
        };
        (row_top(idx, &pitches), h)
    };
    let scroll = match *SCROLL_VIEW.lock().unwrap() {
        Some(s) => s.0,
        None => return,
    };
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    if clip.is_null() {
        return;
    }
    let bounds: NSRect = msg_send![clip, bounds];
    let document: NSRect = msg_send![container, frame];
    let target = selection_scroll_offset(
        bounds.origin.y,
        bounds.size.height,
        document.size.height,
        y,
        h,
    );
    if (target - bounds.origin.y).abs() > f64::EPSILON {
        // scrollPoint:使用文档视图坐标;显式 clamp 后不会因快速重复事件被旧位置覆盖。
        // scrollPoint: uses document-view coordinates; explicit clamping prevents rapid
        // repeated events from being overwritten by a stale position.
        let _: () = msg_send![container, scrollPoint: NSPoint::new(0.0, target)];
    }
}

/// 更新选中高亮,只刷新前后两行的视觉状态,不重建列表。
/// Refresh selection highlight by updating only the previous and new rows, without rebuilding.
pub(super) fn refresh_selection(previous: usize, current: usize) {
    update_hover_visuals(previous, current);
}

extern "C" fn container_accepts_first_responder(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}

extern "C" fn picker_window_can_become_key(_self: *mut c_void, _cmd: Sel) -> bool {
    true
}
