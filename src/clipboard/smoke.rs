//! 剪贴板子系统 · 冒烟模式:--smoke-clipboard 启动的端到端 GUI 冒烟。
//! 历史文件与图片缓存重定向到专用目录,绝不触碰真实用户数据。
//!
//! The clipboard subsystem's smoke mode: the --smoke-clipboard end-to-end GUI run.
//! History file and image cache are redirected to a dedicated directory, never
//! touching real user data.

use super::*;

// ========== 冒烟模式 / smoke mode ==========

/// 冒烟模式(--smoke-clipboard):历史文件与图片缓存重定向到专用目录,绝不触碰
/// 真实用户数据(真实二进制运行时 cfg!(test) 不生效,这是唯一的隔离手段)。
/// Smoke mode (--smoke-clipboard): the history file and the image cache are redirected to
/// a dedicated directory, never touching real user data (cfg!(test) is off in the real
/// binary, so this is the only isolation available).
pub(super) static SMOKE_MODE: AtomicBool = AtomicBool::new(false);

/// 开启冒烟模式(main.rs 在 --smoke-clipboard 分支调用,须在 smoke_runner 之前)。
/// Enable smoke mode (called by main.rs on the --smoke-clipboard branch, before
/// smoke_runner).
pub(crate) fn set_smoke_mode() {
    SMOKE_MODE.store(true, Ordering::SeqCst);
}

/// --smoke-clipboard 入口(主线程调用):注入两条历史后连续两次显示/隐藏浮窗,
/// 覆盖 rebuild_rows 的行清理路径——这里曾是二次释放 UAF(第二次呼出 segfault)。
/// 成功返回 true;崩溃(panic/segfault)即失败。
///
/// --smoke-clipboard entry (called on the main thread): inject two entries, then show/hide
/// the picker twice to exercise rebuild_rows' row-cleanup path -- the site of a double-release
/// UAF that once segfaulted on the second summon. Returns true on success; a crash is a failure.
pub(crate) fn smoke_runner() -> bool {
    // 8x8 实心 PNG:图片条目 + 详情预览(.detail 懒生成)共用。注意不能用 1x1 透明图:
    // 那种图在 TIFFRepresentation 重编码时失败(CGImageDestinationFinalize),详情预览
    // 生成路径会走不到"生成并落盘"分支。
    // An 8x8 solid PNG shared by the image entry and the lazy detail preview. Deliberately
    // NOT the 1x1 transparent PNG: that one fails TIFFRepresentation re-encoding
    // (CGImageDestinationFinalize), so the detail-preview generate-and-cache branch would
    // never be exercised.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x4B,
        0x6D, 0x29, 0xDC, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x38,
        0xA1, 0xA1, 0x81, 0x15, 0x31, 0x0C, 0x2D, 0x09, 0x00, 0x82, 0x5D, 0x46, 0x01, 0x6A, 0x8D,
        0x16, 0x6B, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let tiny_hash = fnv1a64(TINY_PNG);
    {
        let mut hist = CLIP_HISTORY.lock().unwrap();
        // 注入 12 条:超出可视行数(10),覆盖滚动文档(NSScrollView)路径。
        // Inject 12 entries: more than the visible rows (10), covering the scroll-document
        // (NSScrollView) path.
        for i in 0..12 {
            record_text(
                &mut hist,
                &format!("smoke entry {i:02}"),
                "Ghostty",
                "com.mitchellh.ghostty",
                50,
            );
        }
        record_text(
            &mut hist,
            "apple pie recipe",
            "Safari",
            "com.apple.Safari",
            50,
        );
        record_text(&mut hist, "banana bread", "Chrome", "com.google.Chrome", 50);
        // 无来源条目:标题栏应显示"未知来源",无图标。
        // A source-less entry: the header shows "unknown source", no icon.
        record_text(&mut hist, "legacy entry without a source", "", "", 50);
        // 长代码紧邻最新图片:→ 打开图片后按 ↓ 会切到真正溢出的软换行详情,覆盖
        // 自定义软换行、完整 TextKit 布局、原生 scroller、顶部复位和两端橡皮筋状态。
        // Long code next to the newest image means Down after opening the image reaches a
        // genuinely overflowing soft-wrapped detail, covering custom wrapping, full TextKit
        // layout, the native scroller, top reset, and rubber-band state at both endpoints.
        record_text(
            &mut hist,
            &"fn detail_scrolling_regression() { let result = service.fetch(first_argument, second_argument) && enabled; }\n".repeat(200),
            "TextEdit",
            "com.apple.TextEdit",
            50,
        );
        // 图片条目:写入测试缓存目录 + 构造引用,覆盖缩略图渲染/清理路径。
        // An image entry: written into the cache dir and referenced, covering the thumbnail
        // render/cleanup paths.
        let _ = cache_write_image(tiny_hash, TINY_PNG);
        let tiny = ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: tiny_hash,
            data_path: clip_image_path(tiny_hash),
            preview_png: TINY_PNG.to_vec(),
            source_path: None,
        };
        record_image(&mut hist, &tiny, "Safari", "com.apple.Safari", 50);
        // 同图再复制一次:内容哈希去重,历史不增长(验证图片去重)。
        // Re-copying the same image: the content-hash dedup keeps the history from growing
        // (exercises image dedup).
        let before = hist.len();
        record_image(&mut hist, &tiny, "Safari", "com.apple.Safari", 50);
        assert_eq!(hist.len(), before, "same image must dedup");
    }
    show_picker();
    hide_picker();
    // 第二次显示:rebuild_rows 会先移除旧行(曾经的 UAF 路径)。
    // Second show: rebuild_rows removes the old rows first (the former UAF path).
    show_picker();
    // 搜索冒烟:设置搜索词 → 重建(过滤显示)→ 方向键在过滤列表内导航 → 清空恢复。
    // Search smoke: set a query -> rebuild (filtered display) -> arrow navigation within the
    // filtered list -> clear restores everything.
    unsafe {
        *SEARCH_QUERY.lock().unwrap() = "apple".to_string();
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // 搜索框 ↓:经 delegate 命令拦截(moveDown:)焦点切到列表并选中第一条(过滤结果
        // 保留)。直接调 handler:真实链路里字段编辑器把 ↓ 翻译成 moveDown: 后调它。
        // Search-field down-arrow: the delegate command interception (moveDown:) moves focus
        // into the list and selects the first filtered entry. The handler is called directly
        // -- in the real chain the field editor translates ↓ to moveDown: and invokes it.
        if let Some(_f) = *SEARCH_FIELD.lock().unwrap() {
            search_field_do_command(
                std::ptr::null_mut(),
                sel!(control:textView:doCommandBySelector:),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                sel!(moveDown:),
            );
        }
        // 列表顶部 ↑:焦点跳回搜索框(搜索词保留);搜索框开始编辑的 delegate 回调
        // (search_field_began_editing)会清除选中 → 高光消失。
        // Up at the list top: focus jumps back to the search field (query kept); the
        // search field's begin-editing delegate callback (search_field_began_editing) clears
        // the selection -> the highlight disappears.
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // 断言:焦点进搜索框后选中被清除(无行高亮)。
        // Assert: after focus enters the search field the selection is cleared (no row
        // highlight).
        assert_eq!(
            *PICKER_SELECTION.lock().unwrap(),
            NO_SELECTION,
            "selection must clear when focus moves into the search field"
        );
        // Esc 第一级:清空搜索词并恢复全列表(列表聚焦路径)。
        // Esc level one: clear the query and restore the full list (list-focus path).
        let ev_esc = make_key_event(53);
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_esc as *mut c_void);
        }
        clear_search();
    }
    // 键盘导航冒烟:构造真实 NSEvent 走 container_key_down,覆盖方向键 → 选中 → 滚动
    // 到可见的完整路径(曾因 scrollRectToVisible: 返回类型编码错误 panic)。
    // Keyboard-navigation smoke: build a real NSEvent and drive container_key_down, covering
    // arrow -> select -> scroll-into-view (once panicked on a wrong return-type encoding for
    // scrollRectToVisible:).
    unsafe {
        // 先取指针再进块:if let 的 scrutinee 临时 MutexGuard 存活到块结束,块内调用
        // container_key_down → rebuild_rows 会重锁 PICKER_CONTAINER,同线程非重入
        // Mutex 直接自死锁(曾导致冒烟挂起;sample 采样确认栈停在 rebuild_rows 的 lock)。
        // Take the pointer first, then enter the block: the if-let scrutinee's temporary
        // MutexGuard lives until the block ends, and container_key_down -> rebuild_rows
        // re-locks PICKER_CONTAINER inside the block -- a self-deadlock on the same thread
        // (the smoke run used to hang; sample confirmed the stack stuck in rebuild_rows' lock).
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            let ev2 = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev2 as *mut c_void);
            // 多次滚动:每次滚动都触发 clipView bounds 变化通知 → 滚动指示器更新路径
            // (setOpacity: 等 msg_send 曾因 float/double 编码错误 panic,冒烟必须覆盖)。
            // Scroll several times: each scroll fires the clip-view bounds-change notification
            // -> the indicator-update path (setOpacity: once panicked on a float/double
            // encoding mismatch; the smoke run must cover it).
            for step in 1..=5u8 {
                let _: () = msg_send![c.0, scrollPoint: NSPoint::new(0.0, step as f64 * 30.0)];
            }
        }
    }
    // 详情/置顶冒烟:→ 展开详情(文本),↓ 跟随刷新(不关闭),← 直接置顶且保持详情,
    // → 关闭详情;图片条目 → 展开详情(懒生成 .detail 大图);Esc 详情打开时第一级 = 关详情。
    // Detail/pin smoke: → opens the detail (text), ↓ follows it (stays open), ← pins while
    // keeping the detail open, and → closes it; an image entry's → opens its detail (lazy
    // .detail generation); with the detail open, Esc's level one closes the detail only.
    unsafe {
        *PICKER_SELECTION.lock().unwrap() = 0;
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            // →:展开详情(显示第 0 条 = 最新录制的图片条目 → 图片大图分支)。
            // Right: expand the detail for display 0 (the newest recorded entry, an image ->
            // the image branch).
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                DETAIL_VISIBLE.load(Ordering::SeqCst),
                "right arrow must open the detail panel"
            );
            // ↓:详情跟随选中条目(不关闭);选中切到文本条目 → 完整文本分支。
            // Down: the detail follows the selection (stays open); the selection moves to a
            // text entry -> the full-text branch.
            let ev = make_key_event(125);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                DETAIL_VISIBLE.load(Ordering::SeqCst),
                "the detail must stay open while navigating"
            );
            // 溢出文本详情必须使用稳定的完整布局 + 原生 scroller,打开即处于 AppKit
            // 真实顶部。再模拟 bounds 越过上下端点,通知回调只刷新胶囊、绝不能改写
            // clipView(橡皮筋是原生 elasticity 的职责)。
            // An overflowing text detail must use stable full layout plus the native scroller
            // and open at AppKit's actual top. Simulate bounds crossing both endpoints; the
            // notification callback only refreshes the capsules and must never rewrite the
            // clip view (rubber banding is native elasticity's job).
            let wrapped_scroll = DETAIL_SCROLL_VIEW
                .lock()
                .unwrap()
                .expect("text detail must install a scroll view")
                .0;
            let wrapped_doc: *mut AnyObject = msg_send![wrapped_scroll, documentView];
            let wrapped_string: *mut AnyObject = msg_send![wrapped_doc, string];
            let wrapped_horizontal: bool = msg_send![wrapped_scroll, hasHorizontalScroller];
            assert!(
                !wrapped_horizontal,
                "soft-wrap detail must remove the horizontal scroller"
            );
            assert!(
                nsstring_to_rust(wrapped_string).contains('\u{2028}'),
                "long code detail must contain custom soft-wrap separators"
            );
            assert!(
                DETAIL_SOURCE_MAP.lock().unwrap().is_some(),
                "custom soft wraps must retain a source map for copying"
            );

            let detail_content = DETAIL_CONTENT.lock().unwrap().unwrap().0;
            let wrap_button = detail_wrap_button(detail_content);
            assert!(
                !wrap_button.is_null(),
                "code detail must install a wrap button"
            );
            // 另存为按钮只验证存在:点击会弹出 NSSavePanel 模态,测试里不能触发。
            // The save-as button is only presence-checked: clicking it would present the
            // NSSavePanel modal loop, which tests must not trigger.
            let save_as_button = detail_save_as_button(detail_content);
            assert!(
                !save_as_button.is_null(),
                "detail toolbar must install a save-as button"
            );
            let _: () = msg_send![wrap_button, performClick: std::ptr::null::<AnyObject>()];
            let no_wrap_scroll = DETAIL_SCROLL_VIEW
                .lock()
                .unwrap()
                .expect("no-wrap detail must rebuild its scroll view")
                .0;
            let no_wrap_doc: *mut AnyObject = msg_send![no_wrap_scroll, documentView];
            let no_wrap_string: *mut AnyObject = msg_send![no_wrap_doc, string];
            let no_wrap_horizontal: bool = msg_send![no_wrap_scroll, hasHorizontalScroller];
            let no_wrap_frame: NSRect = msg_send![no_wrap_doc, frame];
            let no_wrap_clip: *mut AnyObject = msg_send![no_wrap_scroll, contentView];
            let no_wrap_bounds: NSRect = msg_send![no_wrap_clip, bounds];
            assert!(
                !no_wrap_horizontal,
                "no-wrap mode must use only the custom horizontal indicator"
            );
            assert!(
                DETAIL_HORIZONTAL_SCROLL_INDICATOR.lock().unwrap().is_some(),
                "no-wrap mode must install a custom horizontal indicator"
            );
            assert!(
                !nsstring_to_rust(no_wrap_string).contains('\u{2028}'),
                "no-wrap mode must display the untouched source"
            );
            // 非软换行模式同样携带映射(段内中点的复制保真依赖它),且显示文本
            // 应含中点标记、不含真实空格以外的 U+2028。
            assert!(DETAIL_SOURCE_MAP.lock().unwrap().is_some());
            assert!(
                no_wrap_frame.size.width > no_wrap_bounds.size.width,
                "long source lines must overflow the horizontal viewport"
            );

            // 还原默认软换行,继续验证完整布局和垂直端点。
            // Restore the default soft-wrap state before checking full layout and vertical bounds.
            let no_wrap_button = detail_wrap_button(detail_content);
            assert!(
                !no_wrap_button.is_null(),
                "rebuilt code detail must retain its wrap button"
            );
            let _: () = msg_send![no_wrap_button, performClick: std::ptr::null::<AnyObject>()];
            let detail_scroll = DETAIL_SCROLL_VIEW
                .lock()
                .unwrap()
                .expect("wrapped detail must rebuild its scroll view")
                .0;
            let detail_clip: *mut AnyObject = msg_send![detail_scroll, contentView];
            let (min_y, max_y) =
                detail_scroll_range(detail_scroll).expect("detail must have a legal range");
            assert!(max_y > min_y, "long detail must overflow");
            let opened_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                opened_bounds.origin.y, min_y,
                "detail must open at its real top"
            );
            let has_scroller: bool = msg_send![detail_scroll, hasVerticalScroller];
            let has_horizontal_scroller: bool = msg_send![detail_scroll, hasHorizontalScroller];
            let autohides: bool = msg_send![detail_scroll, autohidesScrollers];
            let scroller_style: isize = msg_send![detail_scroll, scrollerStyle];
            let elasticity: isize = msg_send![detail_scroll, verticalScrollElasticity];
            let detail_doc: *mut AnyObject = msg_send![detail_scroll, documentView];
            let layout: *mut AnyObject = msg_send![detail_doc, layoutManager];
            let noncontiguous: bool = msg_send![layout, allowsNonContiguousLayout];
            let background: bool = msg_send![layout, backgroundLayoutEnabled];
            assert!(
                !has_scroller,
                "detail must disable the native vertical scroller"
            );
            assert!(
                !has_horizontal_scroller,
                "soft-wrap detail must not use a horizontal scroller"
            );
            assert!(
                autohides,
                "detail keeps native scroller auto-hide enabled as a defensive fallback"
            );
            assert_eq!(scroller_style, 1, "detail must use overlay scrollers");
            // 0 = NSScrollElasticityAutomatic:端点橡皮筋是原生职责,bounds 通知里
            // 严禁改写 clipView(硬钳会与动量拉锯导致滚动条抽搐)。
            // 0 = NSScrollElasticityAutomatic: endpoint rubber banding is native; the
            // bounds notification must never rewrite the clip view (hard-clamping fights
            // momentum and twitches the scrollbar).
            assert_eq!(
                elasticity, 0,
                "detail must keep native automatic rubber-band elasticity"
            );
            assert!(!noncontiguous, "detail layout must be contiguous");
            assert!(!background, "detail background layout must be disabled");
            // 越界原点是橡皮筋的合法状态:bounds 通知回调只刷新胶囊,绝不能改写
            // clipView——两端各验证一次"回调后越界原点保持原样"。
            // Out-of-range origins are legal rubber-band state: the notification callback
            // only refreshes capsules and must not touch the clip view -- verified at both
            // endpoints by asserting the overscrolled origin survives the callback.
            let _: () = msg_send![
                detail_clip,
                setBoundsOrigin: NSPoint::new(opened_bounds.origin.x, min_y - 30.0)
            ];
            detail_scroll_indicator_bounds_changed(
                observer() as *mut c_void,
                sel!(detailScrollIndicatorBoundsChanged:),
                std::ptr::null_mut(),
            );
            let top_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                top_bounds.origin.y,
                min_y - 30.0,
                "top overscroll belongs to native rubber banding and must survive"
            );
            let _: () = msg_send![
                detail_clip,
                setBoundsOrigin: NSPoint::new(opened_bounds.origin.x, max_y + 30.0)
            ];
            detail_scroll_indicator_bounds_changed(
                observer() as *mut c_void,
                sel!(detailScrollIndicatorBoundsChanged:),
                std::ptr::null_mut(),
            );
            let bottom_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                bottom_bounds.origin.y,
                max_y + 30.0,
                "bottom overscroll belongs to native rubber banding and must survive"
            );
            scroll_detail_to_top(detail_scroll);
            // ←:详情打开时直接置顶当前选中条目,详情保持打开并跟随重排后的位置。
            // Left: pin the selected entry while the detail stays open and follows its new row.
            let pinned_text = {
                let sel_idx = *PICKER_SELECTION.lock().unwrap();
                let hist = CLIP_HISTORY.lock().unwrap();
                mapped_index(sel_idx)
                    .and_then(|h| hist.get(h))
                    .map(|e| e.text.clone())
            };
            let ev = make_key_event(123);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                DETAIL_VISIBLE.load(Ordering::SeqCst),
                "left arrow must keep the detail panel open"
            );
            {
                let hist = CLIP_HISTORY.lock().unwrap();
                assert!(
                    pinned_text
                        .as_deref()
                        .is_some_and(|t| hist.iter().any(|e| e.pinned && e.text == t)),
                    "left arrow must pin the selected entry"
                );
            }
            // →:详情打开时关闭详情。
            // Right with the detail open: close the detail panel.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !DETAIL_VISIBLE.load(Ordering::SeqCst),
                "right arrow must close the detail when it is open"
            );
            // →:再次展开详情,再按 → 关闭,覆盖详情 toggle 路径。
            // Reopen with →, then close with → again to cover the detail toggle path.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(DETAIL_VISIBLE.load(Ordering::SeqCst));
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !DETAIL_VISIBLE.load(Ordering::SeqCst),
                "right arrow must close the detail when it is open"
            );
            // Esc:详情打开时第一级 = 关闭详情,浮窗与搜索词保持。
            // Esc with the detail open: level one closes the detail; the picker stays.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(DETAIL_VISIBLE.load(Ordering::SeqCst));
            let ev = make_key_event(53);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !DETAIL_VISIBLE.load(Ordering::SeqCst),
                "Esc must close the detail first"
            );
        }
        // 图片条目:定位其显示索引,→ 展开详情 → .detail 大图懒生成落盘。
        // Image entry: locate its display index, → expands it -> the lazy .detail preview is
        // generated and cached.
        let img_h = {
            let hist = CLIP_HISTORY.lock().unwrap();
            hist.iter().position(|e| e.image.is_some())
        };
        if let Some(h_idx) = img_h {
            let d_idx = FILTERED.lock().unwrap().iter().position(|&h| h == h_idx);
            if let Some(d_idx) = d_idx {
                *PICKER_SELECTION.lock().unwrap() = d_idx;
                rebuild_rows();
                let c_opt = *PICKER_CONTAINER.lock().unwrap();
                if let Some(c) = c_opt {
                    let ev = make_key_event(124);
                    container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
                    assert!(
                        DETAIL_VISIBLE.load(Ordering::SeqCst),
                        "image detail must open"
                    );
                    assert!(
                        cache_read_detail_preview(tiny_hash).is_some(),
                        "the lazy .detail preview must be generated on first open"
                    );
                }
            }
        }
    }
    hide_picker();
    true
}

/// 构造一个方向键 NSEvent(冒烟用)。/ Build an arrow-key NSEvent (for the smoke run).
unsafe fn make_key_event(keycode: u16) -> *mut AnyObject {
    let chars = make_nsstring("x");
    // keyEventWithType: 参数依次为 NSEventType(unsigned long)、location、modifierFlags、
    // timestamp、windowNumber(NSInteger)、context、characters、charactersIgnoringModifiers、
    // isARepeat、keyCode(unsigned short)。
    // keyEventWithType: takes NSEventType (unsigned long), location, modifierFlags, timestamp,
    // windowNumber (NSInteger), context, characters, charactersIgnoringModifiers, isARepeat,
    // keyCode (unsigned short).
    let ev: *mut AnyObject = msg_send![
        class!(NSEvent),
        keyEventWithType: 10u64,
        location: NSPoint::new(0.0, 0.0),
        modifierFlags: 0u64,
        timestamp: 0.0f64,
        windowNumber: 0isize,
        context: std::ptr::null::<AnyObject>(),
        characters: chars,
        charactersIgnoringModifiers: chars,
        isARepeat: false,
        keyCode: keycode
    ];
    CFRelease(chars as *const c_void);
    ev
}
