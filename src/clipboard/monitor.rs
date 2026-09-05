//! 剪贴板子系统 · monitor:轮
//! 询

use super::*;

// ========== 轮询 / polling ==========

/// 轮询一次:changeCount 变化时读文本入历史。
/// Poll once: read the text into history when changeCount changed.
pub(super) fn poll_clipboard() {
    // 总开关关闭时停止记录(timer 已被 stop() 停掉,但全局通知观察者仍在,
    // 回调必须自行检查——否则关闭后历史还在后台累积)。
    // Stop recording when the master switch is off (stop() kills the timer, but the
    // process-wide pasteboard notification observer stays registered, so the callback must
    // check the switch itself -- otherwise history keeps accumulating while disabled).
    if !CONFIG.read().unwrap().clipboard.enabled {
        return;
    }
    let changed = unsafe {
        let pb: *mut AnyObject = msg_send![class!(NSPasteboard), generalPasteboard];
        if pb.is_null() {
            return;
        }
        let cc: i64 = msg_send![pb, changeCount];
        let mut last = LAST_CHANGE_COUNT.lock().unwrap();
        if *last == cc {
            return;
        }
        let prev = *last;
        *last = cc;
        log_debug!("[clip] pasteboard changeCount {} -> {}", prev, cc);
        true
    };
    if !changed {
        return;
    }
    // 敏感标记拦截:密码管理器等打上 ConcealedType/TransientType 的内容直接跳过,
    // 不进历史(内存与磁盘都不落)。这是 nspasteboard.org 协议的行业标准做法。
    // Sensitive-marker interception: content stamped ConcealedType/TransientType by
    // password managers is skipped entirely -- it never enters the history (neither in
    // memory nor on disk). The industry-standard nspasteboard.org convention.
    if unsafe { pasteboard_has_sensitive_marker() } {
        log_debug!("[clip] change skipped: sensitive/transient marker on pasteboard");
        return;
    }
    // 自家粘贴写回拦截:"使用后移到最前"关闭时,带自家标记的 changeCount 变化
    // (即我们自己的粘贴写回)跳过记录——否则去重移前会把刚用过的条目提到最顶,
    // 历史顺序被"使用"污染。真实复制会 clearContents 清掉标记,不受影响。
    // Own-paste write-back interception: when "move used entries to top" is off, a
    // changeCount bump carrying our marker (our own paste write-back) is skipped --
    // otherwise the dedup-move would bring the just-used entry to the top, polluting the
    // copy-order with usage. A real copy clears the pasteboard and the marker with it.
    if should_skip_paste_writeback(move_used_to_top(), unsafe { pasteboard_has_paste_marker() }) {
        log_debug!("[clip] change skipped: our own paste write-back (move_used_to_top off)");
        return;
    }
    match unsafe { read_pasteboard_text() } {
        Some(text) => {
            // 来源 = 复制瞬间的前台应用(始终记录;显示与否由 CONFIG 的
            // clipboard.show_source_app 决定)。轮询间隔(0.5s)内切应用可能记错来源,
            // 通知路径(changeCount 变化即时回调)则基本精确,第一版接受这个误差。
            // 图标缓存键 = resolve_app_identity 的 key(与切换器同一套回退);
            // 顺带提取 16pt 小图标(app 此刻存活,提取最可靠;失败不影响记录)。
            // Source = the frontmost app at copy time (always recorded; whether it is shown
            // is gated by CONFIG.clipboard.show_source_app). Switching apps within the 0.5s
            // poll interval can misattribute; the notification path (immediate callback) is
            // mostly accurate -- acceptable for v1. The icon-cache key comes from
            // resolve_app_identity (same fallbacks as the switcher); the 16pt small icon is
            // extracted here too (the app is alive now -- the most reliable moment; failure
            // only means no icon).
            let (source, pid) = crate::ffi::frontmost_app_info();
            let source_key = if pid > 0 {
                let id = unsafe { crate::window_collector::resolve_app_identity(pid) };
                let key = id.key.clone();
                let _ = crate::icon_cache::extract_small_icon(pid);
                key
            } else {
                String::new()
            };
            let mut hist = CLIP_HISTORY.lock().unwrap();
            // 图片文件复制(Finder 里 Cmd+C 图片文件):识别为文件复制 → 记成图片条目;
            // 否则按普通文本记录。
            // An image-FILE copy (Cmd+C on an image file in Finder): recognized as a file
            // copy -> recorded as an image entry; otherwise recorded as plain text.
            // 剪贴板带文件 URL(文件复制):本应用只支持**单个图片文件**的复制
            // (记成图片条目);其他文件复制——非图片文件、多文件选择——一律跳过,
            // 否则文件名文本会被当成普通文本记进历史。
            // A file-url on the pasteboard means a FILE copy: this app only supports a
            // SINGLE image-file copy (recorded as an image entry); every other file copy
            // -- non-image files, multi-file selections -- is skipped entirely, so the
            // filename text never leaks into the history as plain text.
            let has_file_url = unsafe { pasteboard_has_file_url() };
            let file_img = if has_file_url {
                unsafe { file_copy_image(&text) }
            } else {
                None
            };
            if let Some(img) = &file_img {
                if record_image(&mut hist, img, &source, &source_key, max_entries()) {
                    // 文件复制只记录类型和计数,不记录来源路径或文件内容。
                    // File copies log only their type and count, never the source path or content.
                    log_debug!(
                        "[clip] recorded file ref (uti={}, total {})",
                        img.uti,
                        hist.len()
                    );
                } else {
                    log_debug!("[clip] change skipped: dup file ref, total {}", hist.len());
                }
            } else if has_file_url {
                // 文件复制但非单个图片文件(非图片文件 / 多文件):跳过,不记录文件名文本。
                // A file copy that isn't a single image file (non-image / multi-file):
                // skipped, the filename text is never recorded.
                log_debug!(
                    "[clip] change skipped: file copy without a single image file ({} chars)",
                    text.chars().count()
                );
            } else if record_text(&mut hist, &text, &source, &source_key, max_entries()) {
                log_debug!(
                    "[clip] recorded text ({} chars, total {})",
                    text.chars().count(),
                    hist.len()
                );
            } else {
                log_debug!(
                    "[clip] change skipped: dup/empty (text {} chars, total {})",
                    text.chars().count(),
                    hist.len()
                );
            }
            // 记录后顺手清理过期条目(懒清理,无额外定时器;呼出时还会再清一次)。
            // Expire right after recording (lazy, no extra timer; the picker summon
            // cleans again).
            expire_entries(&mut hist, now_secs(), ttl_secs());
        }
        // 无文本 → 尝试图片(图文同存时文本优先,第一版取舍)。
        // No text -> try an image (text wins when both are present; a v1 tradeoff).
        None => match unsafe { read_pasteboard_image() } {
            Some(img) => {
                let (source, pid) = crate::ffi::frontmost_app_info();
                let source_key = if pid > 0 {
                    let id = unsafe { crate::window_collector::resolve_app_identity(pid) };
                    let key = id.key.clone();
                    let _ = crate::icon_cache::extract_small_icon(pid);
                    key
                } else {
                    String::new()
                };
                let mut hist = CLIP_HISTORY.lock().unwrap();
                if record_image(&mut hist, &img, &source, &source_key, max_entries()) {
                    log_debug!(
                        "[clip] recorded image (hash={:016x}, uti={}, total {})",
                        img.hash,
                        img.uti,
                        hist.len()
                    );
                    // 录制即预生成详情大图(后台):刚复制的图最可能马上被查看,
                    // 提前落 {hash}.detail 让首开直接秒出高清。deliver=false——
                    // 只为落盘,与选中态无关。
                    // Pregenerate the hi-res detail preview right after recording
                    // (background): a freshly copied image is the most likely one to be
                    // inspected next, so landing {hash}.detail early makes the first
                    // open instantly sharp. deliver=false -- cache only, selection is
                    // irrelevant.
                    request_detail_preview(&img, false);
                } else {
                    log_debug!("[clip] change skipped: dup image (hash={:016x})", img.hash);
                }
                // 记录后顺手清理过期条目(与文本分支一致)。
                // Expire right after recording (same as the text branch).
                expire_entries(&mut hist, now_secs(), ttl_secs());
            }
            None => log_debug!("[clip] change but no text/image (non-pasteboard content?)"),
        },
    }
    // 历史有变更(记录/去重移前/裁剪)→ persist 开启时落盘。
    // The history changed (record/dedup-move/trim) -> persist when enabled.
    save_history();
}

/// timer tick 回调(主线程):继续轮询。
/// Timer tick callback (main thread): keep polling.
pub(super) extern "C" fn clip_poll_tick(_self: *mut c_void, _cmd: Sel, _timer: *mut c_void) {
    poll_clipboard();
}

/// 启动轮询(幂等):创建主线程 NSTimer,并立刻记录一次当前剪贴板。
/// Start polling (idempotent): create a main-thread NSTimer and record the current
/// pasteboard once immediately.
pub(crate) fn start() {
    unsafe {
        let timer_holder = POLL_TIMER.get_or_init(|| Mutex::new(ObjPtr(std::ptr::null_mut())));
        let mut guard = timer_holder.lock().unwrap();
        if !guard.0.is_null() {
            return; // 已在跑 / already running
        }
        // persist 开启:不清缓存(上次会话的图片字节/预览还要用),先加载历史再
        // 记录当前剪贴板;persist 关闭(默认):清空缓存——历史不持久化,残留必为
        // 孤儿;先清后记,避免刚写下的缓存被自己清掉。
        // With persist ON: the cache is kept (the previous session's image bytes/previews
        // are still referenced) and the history is loaded BEFORE recording the current
        // pasteboard. With persist OFF (default): the cache is wiped -- the history is not
        // persisted, so leftovers are orphans; sweeping first keeps the just-written cache
        // from being deleted.
        if persist_enabled() {
            load_history();
        } else {
            clear_clip_image_cache();
        }
        // 启动即清理过期条目(load_history 只覆盖 persist 开启的情况;persist 关闭时
        // 内存历史同样需要过期)。
        // Expire at startup (load_history only covers the persist-on case; the in-memory
        // history needs expiry with persist off too).
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            expire_entries(&mut hist, now_secs(), ttl_secs());
        }
        if persist_enabled() {
            let removed = sweep_current_clip_image_cache();
            if removed > 0 {
                log_debug!("[clip] swept {} orphan image cache files", removed);
            }
            // 存量图片条目补投详情大图预生成(后台):重启后 {hash}.detail 可能尚不存在
            // (上次会话没开过详情),提前生成让首次打开即高清。
            // Warm up detail previews for restored image entries (background): after a
            // restart {hash}.detail may not exist yet (the last session never opened that
            // detail), so generating ahead keeps the first open instantly sharp.
            for entry in CLIP_HISTORY.lock().unwrap().iter() {
                if let Some(img) = &entry.image {
                    request_detail_preview(img, false);
                }
            }
        }
        // 再记录当前剪贴板,否则首次呼出历史为空。
        // Then record the current pasteboard, or the first summon would show an empty list.
        poll_clipboard();
        // 注册剪贴板变化通知:每次变化即时记录,轮询间隔内的快速连续复制不丢失。
        // Register the pasteboard-change notification: instant recording on every change, so
        // rapid consecutive copies between polling samples are not lost.
        register_pasteboard_observer();
        let timer: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: POLL_INTERVAL,
            target: timer_target(),
            selector: sel!(clipPollTick:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: true
        ];
        *guard = ObjPtr(timer);
        log_debug!(
            "Clipboard history polling started (every {}s).",
            POLL_INTERVAL
        );
    }
}

/// 停止轮询(幂等)。/ Stop polling (idempotent).
pub(crate) fn stop() {
    unsafe {
        let timer_holder = POLL_TIMER.get_or_init(|| Mutex::new(ObjPtr(std::ptr::null_mut())));
        let mut guard = timer_holder.lock().unwrap();
        if !guard.0.is_null() {
            let _: () = msg_send![guard.0, invalidate];
            // scheduledTimerWithTimeInterval: 返回 +0(runloop 持有),不能 release
            // (over-release 会崩溃);invalidate 后 runloop 自行释放。
            // scheduledTimerWithTimeInterval: returns +0 (owned by the run loop); it must
            // NOT be released (over-release crashes); invalidate lets the run loop release it.
            *guard = ObjPtr(std::ptr::null_mut());
            log_debug!("Clipboard history polling stopped.");
        }
    }
}
