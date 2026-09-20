//! 设置窗口 · window:窗口类/生命周期(显示、隐藏、侧栏切换)与内容构建。
//! Settings window classes and lifecycle (show/hide/sidebar switching) plus content construction.

use super::*;

/// 切换侧边栏选中页:高亮背景对齐到选中按钮、切换七个内容视图显隐、选中项粗体。
/// Switch the active settings page: align the highlight to the selected button, toggle the
/// seven content views' visibility, and bold the selected item's label.
pub(super) fn select_sidebar(idx: usize) {
    // tag 越界时回退到通用页 / fall back to the General page if the tag is out of range
    let idx = if idx > 6 { 0 } else { idx };
    // 切换页面时清理上一页的禁用提示，避免提示气泡跨 Tab 残留。
    // Dismiss the previous page's disabled hint so the bubble cannot remain across tabs.
    unsafe { tooltip::SettingsTooltip::dismiss() };
    let previous_idx = SIDEBAR_SELECTED.swap(idx, Ordering::SeqCst);
    unsafe {
        with_settings_ui(|ui| {
            let ui = match ui.as_ref() {
                Some(u) => u,
                None => return,
            };
            let buttons = [
                ui.sidebar_general,
                ui.sidebar_switcher,
                ui.sidebar_mouse,
                ui.sidebar_clipboard,
                ui.sidebar_window_control,
                ui.sidebar_quick_actions,
                ui.sidebar_about,
            ];
            let views = [
                ui.general_view,
                ui.switcher_view,
                ui.mouse_view,
                ui.clipboard_view,
                ui.window_control_view,
                ui.quick_actions_view,
                ui.about_view,
            ];
            // 高亮背景对齐到选中按钮的 frame / align the highlight to the selected button's frame
            let frame: NSRect = msg_send![buttons[idx], frame];
            // 鼠标已经停在目标 tab 上时,悬停背景已完成定位；点击只需同步选中态,避免重复播放
            // 一段明显的位移动画。键盘切换或非悬停切换仍保留完整 spring。
            // When the pointer is already over the target tab, the hover background is in place;
            // clicking only synchronizes selection instead of replaying a conspicuous glide.
            // Keyboard and non-hovered selection changes keep the full spring.
            let target_is_hovered = widgets::sidebar_button_is_hovered(buttons[idx]);
            SettingsSidebar::move_highlight(
                ui.sidebar_highlight,
                frame,
                previous_idx != idx && !target_is_hovered,
            );
            // 选中项使用强调色粗体，未选中项使用系统常规文本色。
            // Selected items use an accent-colored bold title; unselected items use the system label color.
            let titles = [
                t("settings.sidebar_general"),
                t("settings.sidebar_switcher"),
                t("settings.sidebar_mouse"),
                t("settings.sidebar_clipboard"),
                t("settings.sidebar_window_control"),
                t("settings.sidebar_quick_actions"),
                t("settings.sidebar_about"),
            ];
            for (i, &b) in buttons.iter().enumerate() {
                let layer: *mut AnyObject = msg_send![b, layer];
                if !layer.is_null() {
                    layer_set_background(layer, crate::ffi::hex_to_cg_color(0x00000000u32));
                }
                set_sidebar_title(b, &titles[i], i == idx);
            }
            // 切换七页显隐 / toggle the seven pages' visibility
            for (i, &v) in views.iter().enumerate() {
                let _: () = msg_send![v, setHidden: i != idx];
            }
            // 刚显示的页(如从隐藏切出来)需先排版,clip bounds 才会正确,随后滚到顶部。
            // A just-shown page needs a layout pass first so the clip bounds are correct;
            // then scroll it to the top. layoutIfNeeded lives on the window, not the scroll view.
            let _: () = msg_send![ui.window, layoutIfNeeded];
            let page = SettingsPage {
                scroll: views[idx],
                document: msg_send![views[idx], documentView],
            };
            page.scroll_to_top();
            let page_names = [
                "general",
                "switcher",
                "mouse",
                "clipboard",
                "window-control",
                "quick-actions",
                "about",
            ];
            page.validate(page_names[idx]);
        });
    }
}

/// 红绿灯偏移常量:恢复原来的右下偏移位置。
/// 窗口坐标 y 向上,右下 = x 增大 / y 减小。
/// Traffic-light offset: restore the original down-right position.
/// Window coordinates point up, so down-right = x+ / y-.
const TRAFFIC_LIGHT_DX: f64 = 8.0;
const TRAFFIC_LIGHT_DY: f64 = -6.0;
static TRAFFIC_LIGHT_BASE_ORIGINS: LazyLock<Mutex<HashMap<usize, [Option<NSPoint>; 3]>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 把三个红绿灯按钮往右下偏移:通过公开 API standardWindowButton: 拿到按钮视图直接改 frame
/// (没有公开 API 直接设红绿灯位置,旧私有 API setTrafficLightPosition: 等在 macOS 26 已移除,
/// 实测这是唯一可靠的做法)。
/// 注意:两参的 +standardWindowButton:forStyleMask: 是类方法,发给实例会被 objc2 的方法
/// 检查拦截崩掉(此前踩过的坑);必须用一参的实例方法 -standardWindowButton:。
/// 必须在窗口完成首次布局之后调用 —— 布局前移动会被 AppKit 重置;resize 也会重置,
/// 所以每次 show 和 resize 后都要重放(见 show_settings 与 resizeSubviewsWithOldSize:)。
/// 首次布局时记录系统原始坐标,后续始终从原始坐标计算,避免重复 show/resize 时累积偏移。
/// 按钮为 nil 时静默跳过,旧版 macOS 同样适用。
///
/// Nudge the three traffic-light buttons down-right: grab the button views via the public
/// -standardWindowButton: and move their frames (no public API sets the traffic light position;
/// the old private setTrafficLightPosition: etc. are gone on macOS 26, and this is the only
/// reliable way -- verified on this machine). Note: the two-arg +standardWindowButton:forStyleMask:
/// is a CLASS method; sending it to an instance trips objc2's method check and panics (a pitfall
/// we hit) -- the one-arg instance method -standardWindowButton: must be used. Must run after the
/// window's first layout pass: moves before layout are reset by AppKit, and resize also resets
/// them, so the offset is re-applied on every show and resize (see show_settings and
/// resizeSubviewsWithOldSize:). Original positions are captured once per window so repeated
/// calls remain idempotent instead of accumulating the offset. Skips silently when a button is
/// nil; works on older macOS too.
pub(super) unsafe fn reposition_traffic_lights(window: *mut AnyObject) {
    // NSWindowButton: Close=0, Miniaturize=1, Zoom=2
    let base_origins = {
        let mut all_origins = TRAFFIC_LIGHT_BASE_ORIGINS.lock().unwrap();
        let origins = all_origins
            .entry(window as usize)
            .or_insert([None, None, None]);
        for tag in 0..3isize {
            let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
            if !btn.is_null() && origins[tag as usize].is_none() {
                let f: NSRect = msg_send![btn, frame];
                origins[tag as usize] = Some(f.origin);
            }
        }
        *origins
    };
    for tag in 0..3isize {
        let btn: *mut AnyObject = msg_send![window, standardWindowButton: tag];
        if let (false, Some(base_origin)) = (btn.is_null(), base_origins[tag as usize]) {
            let _: () = msg_send![
                btn,
                setFrameOrigin: NSPoint::new(
                    base_origin.x + TRAFFIC_LIGHT_DX,
                    base_origin.y + TRAFFIC_LIGHT_DY
                )
            ];
        }
    }
}

/// 将设置窗口居中到当前鼠标所在屏幕的可用区域,排除菜单栏和 Dock。
/// Center the settings window in the visible area of the screen under the cursor, excluding the
/// menu bar and Dock.
unsafe fn center_settings_window(window: *mut AnyObject) {
    let cursor: NSPoint = msg_send![class!(NSEvent), mouseLocation];
    let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
    let count: usize = msg_send![screens, count];
    let mut visible_frame: Option<NSRect> = None;

    for i in 0..count {
        let screen: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
        let frame: NSRect = msg_send![screen, frame];
        if cursor.x >= frame.origin.x
            && cursor.x <= frame.origin.x + frame.size.width
            && cursor.y >= frame.origin.y
            && cursor.y <= frame.origin.y + frame.size.height
        {
            visible_frame = Some(msg_send![screen, visibleFrame]);
            break;
        }
    }

    let visible = visible_frame.unwrap_or_else(|| {
        let main: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        msg_send![main, visibleFrame]
    });
    let window_frame: NSRect = msg_send![window, frame];
    let origin = NSPoint::new(
        visible.origin.x + (visible.size.width - window_frame.size.width) / 2.0,
        visible.origin.y + (visible.size.height - window_frame.size.height) / 2.0,
    );
    let _: () = msg_send![window, setFrameOrigin: origin];
}

pub(super) fn show_settings() {
    show_settings_inner(None, 0, None, true);
}

/// Show the settings window, optionally preserving its frame and selected page.
/// 显示设置窗口,可选地保留窗口位置和当前页。
fn show_settings_preserving(frame: NSRect, page: usize, scroll_offsets: [NSPoint; 7]) {
    show_settings_inner(Some(frame), page, Some(scroll_offsets), false);
}

unsafe fn capture_settings_scroll_offsets(ui: &SettingsUi) -> [NSPoint; 7] {
    let scrolls = [
        ui.general_view,
        ui.switcher_view,
        ui.mouse_view,
        ui.clipboard_view,
        ui.window_control_view,
        ui.quick_actions_view,
        ui.about_view,
    ];
    scrolls.map(|scroll| {
        let clip: *mut AnyObject = msg_send![scroll, contentView];
        let bounds: NSRect = msg_send![clip, bounds];
        bounds.origin
    })
}

unsafe fn restore_settings_scroll_offset(scroll: *mut AnyObject, origin: NSPoint) {
    if scroll.is_null() {
        return;
    }
    let clip: *mut AnyObject = msg_send![scroll, contentView];
    let document: *mut AnyObject = msg_send![scroll, documentView];
    if clip.is_null() || document.is_null() {
        return;
    }
    let clip_bounds: NSRect = msg_send![clip, bounds];
    let document_frame: NSRect = msg_send![document, frame];
    let max_x = (document_frame.size.width - clip_bounds.size.width).max(0.0);
    let max_y = (document_frame.size.height - clip_bounds.size.height).max(0.0);
    let target = NSPoint::new(origin.x.clamp(0.0, max_x), origin.y.clamp(0.0, max_y));
    let _: () = msg_send![clip, scrollToPoint: target];
    let _: () = msg_send![scroll, reflectScrolledClipView: clip];
}

fn show_settings_inner(
    preserved_frame: Option<NSRect>,
    page: usize,
    preserved_scroll_offsets: Option<[NSPoint; 7]>,
    present_window: bool,
) {
    unsafe {
        {
            let missing = with_settings_ui(|ui| ui.is_none());
            if missing {
                create_settings_window();
            }
        }
        // 每次打开设置都从收起态开始,避免上次未完成的确认状态残留(两套确认卡片)。
        // Always reopen in the collapsed state so no unfinished confirmation lingers (both cards).
        collapse_restore_confirmations(false);
        // A window order-out is not guaranteed to deliver mouseExited for its tracking areas;
        // clear the shared hover state before reusing the settings window.
        // 窗口 orderOut 不保证会为 tracking area 发送 mouseExited；复用设置窗口前先清理共享
        // 悬停状态，避免旧条目在重新打开后留下幽灵高亮。
        widgets::clear_sidebar_hover();
        load_settings_values();
        // 普通打开回到通用页;无缝刷新时保留用户当前页。
        // Normal opens return to General; seamless refreshes preserve the current page.
        select_sidebar(page);
        with_settings_ui(|ui| {
            if let Some(u) = ui.as_ref() {
                if present_window {
                    // 切到 .regular:让设置窗口能正常激活抬升(从别的 App 顶部弹出来),关闭时切回。
                    // Switch to .regular so the settings window can activate and raise itself above
                    // the active app; reverted on close.
                    crate::set_settings_activation_policy(true);
                    let nsapp: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                    let _: () = msg_send![nsapp, activateIgnoringOtherApps: true];
                    if let Some(frame) = preserved_frame {
                        // 在窗口仍隐藏时设置 frame,避免新窗口先出现在默认位置再跳到旧位置。
                        // Set the frame while the replacement is hidden so it never visibly jumps from
                        // its default position to the preserved position.
                        let _: () = msg_send![u.window, setFrame: frame, display: false];
                    } else {
                        center_settings_window(u.window);
                    }
                    let _: () =
                        msg_send![u.window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
                    // 条件行在窗口显示后再算一次:AppKit 首次显示窗口时可能按 autoresizing
                    // 重排子视图,把加载阶段（窗口仍隐藏）做的补位冲掉。组件按实时 frame 判断
                    // 当前状态,重复调用是幂等的,不会二次位移。
                    // Recompute the conditional rows after the window is on screen: AppKit may
                    // re-place subviews by their autoresizing masks the first time a window is
                    // displayed, undoing the compaction done while it was still hidden. The
                    // component derives its state from the live frames, so this call is idempotent
                    // and never shifts twice.
                    update_conditional_rows(u);
                } else if let Some(frame) = preserved_frame {
                    // 主题刷新只更新同一窗口的内容,保持其后台层级与焦点,不触发应用激活。
                    // Theme refresh only replaces content in the same window, preserving its
                    // background order and focus without activating the app.
                    let _: () = msg_send![u.window, setFrame: frame, display: false];
                }
                // 红绿灯偏移:必须等窗口完成首次布局后再移动,否则会被 AppKit 重置。
                // Offset the traffic lights only after the window's first layout pass, or AppKit
                // resets them.
                let _: () = msg_send![u.window, layoutIfNeeded];
                reposition_traffic_lights(u.window);
                if let Some(offsets) = preserved_scroll_offsets {
                    let scrolls = [
                        u.general_view,
                        u.switcher_view,
                        u.mouse_view,
                        u.clipboard_view,
                        u.window_control_view,
                        u.quick_actions_view,
                        u.about_view,
                    ];
                    for (scroll, origin) in scrolls.into_iter().zip(offsets) {
                        restore_settings_scroll_offset(scroll, origin);
                    }
                } else {
                    // Scroll the visible page (General on open) to the top after layout, so the
                    // scrollbar starts at the top of the track instead of the middle.
                    scroll_page_to_top(u.general_view);
                }
                if present_window {
                    // 清掉默认 first responder,避免打开时焦点落在 Glass color 控件。
                    // Clear the default first responder so focus does not land on the Glass color control on open.
                    let _: bool =
                        msg_send![u.window, makeFirstResponder: std::ptr::null::<AnyObject>()];
                    set_text_input_active(false);
                }
                // 按当前权限刷新警告条显隐(有权限就隐藏)/ refresh banner visibility by current permission
                let _: () = msg_send![u.accessibility_warning_view, setHidden: has_accessibility_permission()];
            }
        });
    }
}

pub(super) fn hide_settings() {
    // 防御:关闭设置时若仍在录制/编辑面板,先收尾(复位 RECORDING);关闭编辑面板。
    // Defensive: wrap up any in-progress recording / edit panel when the window closes.
    cancel_recording_from_main();
    close_mapping_panel();
    set_text_input_active(false);
    // 关闭设置时丢弃未确认的恢复动作,下次打开重新从单按钮开始(两套确认卡片)。
    // Closing settings discards any unconfirmed restore action and resets to one button (both
    // cards).
    collapse_restore_confirmations(false);
    let window_and_well = with_settings_ui(|ui| ui.as_ref().map(|u| (u.window, u.glass_tint)));
    unsafe {
        if let Some((window, well)) = window_and_well {
            // orderOut can bypass the sidebar tracking-area exit callback, so do not leave the
            // shared hover pill pointing at a row while the window is hidden.
            // orderOut 可能绕过侧栏 tracking area 的退出回调，窗口隐藏前不能留下仍指向旧条目的
            // 共享悬停气泡。
            widgets::clear_sidebar_hover();
            // 先释放设置锁再关闭颜色面板,通知回调会重新访问 SETTINGS_UI。
            // Release the settings lock before closing the color panel; its notification callback
            // re-enters SETTINGS_UI.
            close_glass_tint_panel(well);
            let _: () = msg_send![window, orderOut: std::ptr::null::<AnyObject>()];
        }
    }
    if SYSTEM_APPEARANCE_REBUILD_PENDING.swap(false, Ordering::SeqCst) {
        invalidate_settings_window();
    }
    // 切回 .accessory:设置窗口关闭,回到纯菜单栏(无 Dock 图标)。
    // Switch back to .accessory: the settings window is closed, return to pure menu-bar (no Dock icon).
    crate::set_settings_activation_policy(false);
}

/// Re-apply the effective appearance from the current CONFIG while preserving page and frame.
///
/// Custom settings layers are painted with concrete palette colors at construction time. When
/// the window is visible, remember the current page and frame, clear and rebuild the content
/// hierarchy inside the same NSWindow, then restore the page and frame. This avoids the visible
/// order-out/reappear gap and the mixed state caused by updating only NSWindow.appearance.
/// Immediate-apply writes CONFIG before calling this, so the redraw already shows the new values.
///
/// 按当前 CONFIG 重新应用外观,同时保留页签与窗口位置。设置页自绘图层在创建时写入具体
/// 调色板颜色,所以窗口可见时先记住当前页与位置,在同一个 NSWindow 内清空并重建内容层级,
/// 再恢复页面和位置,避免先消失再出现的可见空档,也避免只更新 NSWindow appearance 导致
/// 明暗混杂。即时生效路径先写 CONFIG 再调用本函数,因此重绘后的界面已经展示新值。
pub(crate) fn refresh_system_appearance() {
    let (old_window, visible) = with_settings_ui(|ui| {
        ui.as_ref().map(|ui| unsafe {
            let visible: bool = msg_send![ui.window, isVisible];
            (ui.window, visible)
        })
    })
    .unwrap_or((std::ptr::null_mut(), false));

    if old_window.is_null() || !visible {
        invalidate_settings_window();
        return;
    }

    let (page, frame, scroll_offsets) = {
        let page = SIDEBAR_SELECTED.load(Ordering::SeqCst);
        let frame: NSRect = unsafe { msg_send![old_window, frame] };
        let scroll_offsets = with_settings_ui(|ui| {
            ui.as_ref()
                .map(|ui| unsafe { capture_settings_scroll_offsets(ui) })
        })
        .unwrap_or([NSPoint::new(0.0, 0.0); 7]);
        (page, frame, scroll_offsets)
    };

    let old_ui = with_settings_ui(|ui| ui.take());
    let Some(old_ui) = old_ui else {
        invalidate_settings_window();
        return;
    };

    unsafe {
        // Detach callbacks and registries, then redraw the content hierarchy inside the same
        // visible NSWindow. The window never orders out, changes identity, or changes focus.
        // 解除回调和运行时注册表后,在同一个可见 NSWindow 内重绘内容层级;窗口不隐藏、不换对象、
        // 也不改变焦点。
        widgets::clear_sidebar_hover();
        detach_settings_window_runtime(&old_ui, false);
        rebuild_settings_content(old_ui.window);
        show_settings_preserving(frame, page, scroll_offsets);
    }
}

/// Run every settings page through the real AppKit layout path and debug validator, then exit.
/// This is used by the ignored macOS smoke test; it deliberately exercises the same window
/// builder as the interactive app instead of constructing a simplified test-only hierarchy.
/// 在真实 AppKit 布局路径中依次验证所有设置页，然后退出。供 macOS ignored smoke test 使用，
/// 复用交互应用的窗口构建逻辑，不创建简化的测试专用 view tree。
pub(crate) fn settings_layout_smoke_runner() -> bool {
    unsafe {
        show_settings();
        let Some((window, pages)) = with_settings_ui(|ui| {
            ui.as_ref().map(|ui| {
                (
                    ui.window,
                    [
                        ui.general_view,
                        ui.switcher_view,
                        ui.mouse_view,
                        ui.clipboard_view,
                        ui.window_control_view,
                        ui.quick_actions_view,
                        ui.about_view,
                    ],
                )
            })
        }) else {
            return false;
        };
        let _: () = msg_send![window, layoutIfNeeded];
        let opaque: bool = msg_send![window, isOpaque];
        if opaque {
            log_info!("[smoke-settings-layout] settings window unexpectedly opaque");
            hide_settings();
            return false;
        }
        let host: *mut AnyObject = msg_send![window, contentView];
        let root = settings_root_view_for_host(host);
        let root_layer: *mut AnyObject = if root.is_null() {
            std::ptr::null_mut()
        } else {
            msg_send![root, layer]
        };
        let root_clips: bool = if root_layer.is_null() {
            false
        } else {
            msg_send![root_layer, masksToBounds]
        };
        if !root_clips {
            log_info!("[smoke-settings-layout] settings root is not clipping rounded corners");
            hide_settings();
            return false;
        }
        let frame_before: NSRect = msg_send![window, frame];
        let radius_before: f64 = if root_layer.is_null() {
            0.0
        } else {
            msg_send![root_layer, cornerRadius]
        };
        let mut resized_frame = frame_before;
        resized_frame.size.height += 48.0;
        let _: () = msg_send![window, setFrame: resized_frame, display: true];
        let _: () = msg_send![window, layoutIfNeeded];
        refresh_settings_root_corner(window);
        let radius_after: f64 = if root_layer.is_null() {
            0.0
        } else {
            msg_send![root_layer, cornerRadius]
        };
        let radius_stable = radius_before.is_finite()
            && radius_after.is_finite()
            && radius_before >= 0.0
            && radius_after >= 0.0;
        if !radius_stable {
            log_info!("[smoke-settings-layout] root corner radius became invalid after resize");
            hide_settings();
            return false;
        }
        if !root.is_null() && AnyClass::get(c"NSGlassEffectView").is_some() {
            let children: *mut AnyObject = msg_send![root, subviews];
            let child_count: usize = msg_send![children, count];
            let mut glass_content_ok = false;
            for index in 0..child_count {
                let child: *mut AnyObject = msg_send![children, objectAtIndex: index as isize];
                if msg_send![child, isKindOfClass: class!(NSGlassEffectView)] {
                    let glass_content: *mut AnyObject = msg_send![child, contentView];
                    glass_content_ok = !glass_content.is_null();
                    break;
                }
            }
            if !glass_content_ok {
                log_info!("[smoke-settings-layout] Liquid Glass sidebar has no contentView");
                hide_settings();
                return false;
            }
        }
        let names = [
            "general",
            "switcher",
            "mouse",
            "clipboard",
            "window-control",
            "quick-actions",
            "about",
        ];
        for (index, page) in pages.iter().enumerate() {
            select_sidebar(index);
            let _: () = msg_send![window, layoutIfNeeded];
            scroll_page_to_top(*page);
            debug_validate_settings_page(*page, names[index]);
        }
        hide_settings();
        true
    }
}

/// 从窗口切换浮窗关闭本应用的设置窗口,必须在主线程直接执行,不能通过后台 AX 操作回调。
/// Close this app's settings window from the switcher. This must run directly on the main thread,
/// rather than indirectly through a background AX action callback.
pub(crate) fn close_settings_from_switcher() {
    hide_settings();
}

/// 弹一个简单的告警框(app 模态),用于显示校验/保存错误。
/// Show a simple app-modal alert for validation / save errors.
pub(super) fn show_alert(title: &str, msg: &str) {
    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];
        let ns1 = make_nsstring(title);
        let _: () = msg_send![alert, setMessageText: ns1];
        CFRelease(ns1 as *const c_void);
        let ns2 = make_nsstring(msg);
        let _: () = msg_send![alert, setInformativeText: ns2];
        CFRelease(ns2 as *const c_void);
        let ns3 = make_nsstring(&t("alert.btn_ok"));
        let _: *mut AnyObject = msg_send![alert, addButtonWithTitle: ns3];
        CFRelease(ns3 as *const c_void);
        let _resp: isize = msg_send![alert, runModal];
        release_obj(alert);
    }
}

// ========== 窗口生命周期 / window lifecycle 与构建 / construction ==========
struct SettingsWindowClass(*mut AnyObject);
unsafe impl Send for SettingsWindowClass {}
unsafe impl Sync for SettingsWindowClass {}

/// Root view used by the settings window so AppKit can resolve macOS 27's container-relative
/// corner radii while the layer still clips every custom child into the same surface.
/// 设置窗口根视图：让 AppKit 在 macOS 27 上解析相对于窗口的圆角，同时由同一图层裁切所有自绘子视图。
struct SettingsRootViewClass(*mut AnyObject);
unsafe impl Send for SettingsRootViewClass {}
unsafe impl Sync for SettingsRootViewClass {}

static SETTINGS_ROOT_VIEW_CLS: OnceLock<SettingsRootViewClass> = OnceLock::new();

pub(super) fn settings_effective_corner_radius(radii: Option<[f64; 4]>, fallback: f64) -> f64 {
    let Some(radii) = radii else {
        return fallback;
    };
    if radii.iter().all(|radius| radius.is_finite()) {
        radii.iter().copied().fold(0.0, f64::max).max(0.0)
    } else {
        fallback
    }
}

extern "C" fn settings_root_corner_configuration(_self: *mut c_void, _cmd: Sel) -> *mut AnyObject {
    unsafe {
        let Some(radius_cls) = AnyClass::get(c"NSViewCornerRadius") else {
            return std::ptr::null_mut();
        };
        let Some(config_cls) = AnyClass::get(c"NSViewCornerConfiguration") else {
            return std::ptr::null_mut();
        };
        let radius: *mut AnyObject = msg_send![
            radius_cls,
            containerConcentricRadiusWithMinimum: 0.0f64
        ];
        if radius.is_null() {
            return std::ptr::null_mut();
        }
        msg_send![config_cls, configurationWithRadius: radius]
    }
}

extern "C" fn settings_root_view_did_change_effective_corner_radii(this: *mut c_void, _cmd: Sel) {
    unsafe {
        let view = this as *mut AnyObject;
        let radii: *mut AnyObject = msg_send![view, effectiveCornerRadii];
        let radius = if radii.is_null() {
            settings_effective_corner_radius(None, 26.0)
        } else {
            let top_left: f64 = msg_send![radii, topLeft];
            let top_right: f64 = msg_send![radii, topRight];
            let bottom_left: f64 = msg_send![radii, bottomLeft];
            let bottom_right: f64 = msg_send![radii, bottomRight];
            settings_effective_corner_radius(
                Some([top_left, top_right, bottom_left, bottom_right]),
                26.0,
            )
        };
        let layer: *mut AnyObject = msg_send![view, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: radius];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }
}

fn settings_root_view_class() -> *mut AnyObject {
    SETTINGS_ROOT_VIEW_CLS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsRootView").unwrap();
            let superclass = class!(NSView) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            if AnyClass::get(c"NSViewCornerConfiguration").is_some()
                && AnyClass::get(c"NSViewCornerRadius").is_some()
            {
                class_addMethod(
                    cls,
                    sel!(cornerConfiguration),
                    settings_root_corner_configuration as *mut c_void,
                    CString::new("@@:").unwrap().as_ptr(),
                );
                class_addMethod(
                    cls,
                    sel!(viewDidChangeEffectiveCornerRadii),
                    settings_root_view_did_change_effective_corner_radii as *mut c_void,
                    CString::new("v@:").unwrap().as_ptr(),
                );
            }
            objc_registerClassPair(cls);
            SettingsRootViewClass(cls)
        })
        .0
}

unsafe fn settings_root_view_for_host(host: *mut AnyObject) -> *mut AnyObject {
    if host.is_null() {
        return std::ptr::null_mut();
    }
    let subviews: *mut AnyObject = msg_send![host, subviews];
    if subviews.is_null() {
        return std::ptr::null_mut();
    }
    let root_class = settings_root_view_class();
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
        if !subview.is_null() && msg_send![subview, isKindOfClass: root_class] {
            return subview;
        }
    }
    std::ptr::null_mut()
}

unsafe fn settings_root_view_for_window(window: *mut AnyObject) -> *mut AnyObject {
    if window.is_null() {
        return std::ptr::null_mut();
    }
    let host: *mut AnyObject = msg_send![window, contentView];
    settings_root_view_for_host(host)
}

/// Reapply the dynamic corner result after AppKit lays out a resized window.
/// 窗口 resize 后重新应用 AppKit 计算出的动态圆角。
pub(super) unsafe fn refresh_settings_root_corner(window: *mut AnyObject) {
    if AnyClass::get(c"NSViewCornerConfiguration").is_none()
        || AnyClass::get(c"NSViewCornerRadius").is_none()
    {
        return;
    }
    let root = settings_root_view_for_window(window);
    if root.is_null() {
        return;
    }
    let _: () = msg_send![root, invalidateCornerConfiguration];
    let _: () = msg_send![root, layoutSubtreeIfNeeded];
    settings_root_view_did_change_effective_corner_radii(
        root as *mut c_void,
        sel!(viewDidChangeEffectiveCornerRadii),
    );
}

unsafe fn apply_settings_root_surface(
    window: *mut AnyObject,
    content: *mut AnyObject,
    palette: UiPalette,
    fallback_radius: f64,
) {
    let _: () = msg_send![window, setOpaque: false];
    let clear_color: *mut AnyObject = msg_send![class!(NSColor), clearColor];
    let _: () = msg_send![window, setBackgroundColor: clear_color];
    let _: () = msg_send![content, setWantsLayer: true];
    let layer: *mut AnyObject = msg_send![content, layer];
    if layer.is_null() {
        return;
    }
    layer_set_background(layer, crate::ffi::hex_to_cg_color(palette.window_bg));
    let supports_concentric = AnyClass::get(c"NSViewCornerConfiguration").is_some()
        && AnyClass::get(c"NSViewCornerRadius").is_some();
    refresh_settings_root_corner(window);
    if !supports_concentric {
        let _: () = msg_send![layer, setCornerRadius: fallback_radius];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
}

static SETTINGS_WINDOW_CLS: OnceLock<SettingsWindowClass> = OnceLock::new();

fn settings_window_class() -> *mut AnyObject {
    SETTINGS_WINDOW_CLS
        .get_or_init(|| unsafe {
            let name = CString::new("OhMyTabSettingsWindow").unwrap();
            let superclass = class!(NSWindow) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap(); // -performClose:(id)sender -> void
            class_addMethod(
                cls,
                sel!(performClose:),
                settings_window_perform_close as *mut c_void,
                types.as_ptr(),
            );
            let types_close = CString::new("v@:").unwrap(); // -close -> void
            class_addMethod(
                cls,
                sel!(close),
                settings_window_close as *mut c_void,
                types_close.as_ptr(),
            );
            let types_event = CString::new("v@:@").unwrap(); // -sendEvent:(NSEvent*) -> void
            class_addMethod(
                cls,
                sel!(sendEvent:),
                settings_window_send_event as *mut c_void,
                types_event.as_ptr(),
            );
            let types_key = CString::new("B@:@").unwrap(); // -performKeyEquivalent:(NSEvent*) -> BOOL
            class_addMethod(
                cls,
                sel!(performKeyEquivalent:),
                settings_window_perform_key_equivalent as *mut c_void,
                types_key.as_ptr(),
            );
            let types_resize = CString::new("v@:{CGSize=dd}").unwrap(); // -resizeSubviewsWithOldSize:(NSSize) -> void
            class_addMethod(
                cls,
                sel!(resizeSubviewsWithOldSize:),
                settings_window_resize_subviews as *mut c_void,
                types_resize.as_ptr(),
            );
            objc_registerClassPair(cls);
            SettingsWindowClass(cls)
        })
        .0
}

/// Apply the resolved appearance to the settings window and its semantic AppKit controls.
/// 将解析后的主题应用到设置窗口及其依赖语义颜色的 AppKit 控件。
pub(super) unsafe fn apply_settings_window_appearance(window: *mut AnyObject) {
    let name = make_nsstring(if resolved_is_dark() {
        "NSAppearanceNameDarkAqua"
    } else {
        "NSAppearanceNameAqua"
    });
    let appearance: *mut AnyObject = msg_send![class!(NSAppearance), appearanceNamed: name];
    CFRelease(name as *const c_void);
    if !appearance.is_null() {
        let _: () = msg_send![window, setAppearance: appearance];
    }
}

fn create_settings_window() {
    create_settings_window_for(None);
}

/// Rebuild the settings content in an existing window without replacing the window itself.
/// 在现有窗口内重建设置内容,不替换窗口对象本身。
fn rebuild_settings_content(window: *mut AnyObject) {
    create_settings_window_for(Some(window));
}

fn create_settings_window_for(existing_window: Option<*mut AnyObject>) {
    unsafe {
        let palette = settings_palette();
        // Keep the original compact settings window dimensions while applying the redesign's
        // typography, spacing, controls, and grouped-card treatment.
        // 保持原来的紧凑窗口尺寸，同时应用 redesign 的字体、间距、控件和分组卡片风格。
        // Give the redesigned detail pane enough room for full labels, links, and wide fields
        // while keeping the sidebar and the existing window height unchanged.
        let view_w = 820.0;
        let card_margin = 0.0;
        let card_w = SETTINGS_SIDEBAR_WIDTH;
        let window_clip_radius = 26.0;
        let card_radius = 0.0;
        let style: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 3);
        // titled + closable + miniaturizable + resizable(三个红绿灯齐全)。resizable 是绿色 zoom
        // 按钮出现的必要条件;布局是绝对定位不随缩放,故下方用 min=max 固定窗口尺寸。
        // titled + closable + miniaturizable + resizable (all three traffic lights). resizable is
        // required for the green zoom button to appear; the layout is absolute-positioned and
        // doesn't adapt, so the window size is fixed below via min=max.
        // The page content uses generous spacing and grouped cards so the controls remain easy
        // to scan without compressing the taller sections.
        // 初始位置:主显示器(screens[0])居中。不要用 NSScreen mainScreen(其语义是跟随
        // 键盘焦点窗口的屏幕,不是主屏,见 overlay_target_screen 的注释)。
        // Initial position: centered on the primary display (screens[0]). Don't use
        // NSScreen.mainScreen (it follows the key window, not the primary display; see
        // overlay_target_screen's note).
        let win_w = view_w;
        let win_h = 720.0;
        let window = if let Some(window) = existing_window {
            window
        } else {
            let (win_x, win_y) = {
                let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
                let count: usize = msg_send![screens, count];
                if count > 0 {
                    // objectAtIndex: 的参数编码是 'q'(signed long),必须传 isize。
                    // objectAtIndex: expects 'q' (signed long); pass isize.
                    let s: *mut AnyObject = msg_send![screens, objectAtIndex: 0isize];
                    let f: NSRect = msg_send![s, frame];
                    (
                        f.origin.x + (f.size.width - win_w) / 2.0,
                        f.origin.y + (f.size.height - win_h) / 2.0,
                    )
                } else {
                    (220.0, 180.0)
                }
            };
            let frame = NSRect::new(NSPoint::new(win_x, win_y), NSSize::new(win_w, win_h));
            let window: *mut AnyObject = msg_send![settings_window_class(), alloc];
            msg_send![window, initWithContentRect: frame, styleMask: style, backing: 2u64, defer: false]
        };
        apply_settings_window_appearance(window);
        let ns_title = make_nsstring(&t("settings.window_title"));
        let _: () = msg_send![window, setTitle: ns_title];
        CFRelease(ns_title as *const c_void);
        if existing_window.is_none() {
            let _: () = msg_send![window, setReleasedWhenClosed: false];
            // 固定宽度:min/max 宽都等于设计宽度,高度可调 —— 系统设置同款(宽度不能左右调整)。
            // Fixed width: min and max width both equal the designed width, height stays adjustable --
            // same as System Settings (the width cannot be dragged).
            let _: () = msg_send![window, setMinSize: NSSize::new(view_w, 400.0)];
            let _: () = msg_send![window, setMaxSize: NSSize::new(view_w, 10000.0)];

            // 空 unified 工具栏:unified 工具栏(NSWindowToolbarStyleUnified=3)会把窗口主题帧
            // 圆角从 16 提到 26(实测;LinearMouse 的设置窗口就是这么做的),顶部条带随之变为
            // 玻璃材质条带、红绿灯在其中垂直居中。空工具栏不加入任何响应者,不影响
            // performKeyEquivalent:(Cmd+Q)与页面切换。必须在 contentLayoutRect 测量之前设置,
            // 布局高度会自动减去工具栏条带(658 -> 624)。
            // Empty unified toolbar: NSWindowToolbarStyleUnified (3) raises the theme frame's corner
            // radius from 16 to 26 (measured; that's how LinearMouse's settings window does it), and
            // the top strip becomes a glass material strip with the traffic lights centered in it.
            // An empty toolbar adds no responders, so performKeyEquivalent: (Cmd+Q) and page switching
            // are unaffected. Must be set before measuring contentLayoutRect, which then automatically
            // accounts for the toolbar strip (658 -> 624).
            let tb: *mut AnyObject = msg_send![class!(NSToolbar), alloc];
            let tb_id = make_nsstring("OhMyTabSettingsToolbar");
            let tb: *mut AnyObject = msg_send![tb, initWithIdentifier: tb_id];
            CFRelease(tb_id as *const c_void);
            let _: () = msg_send![window, setToolbar: tb];
            let _: () = msg_send![window, setToolbarStyle: 3isize]; // NSWindowToolbarStyleUnified
            release_obj(tb);
        }

        let host: *mut AnyObject = msg_send![window, contentView];
        let content: *mut AnyObject = if existing_window.is_none() {
            let host_bounds: NSRect = msg_send![host, bounds];
            let root: *mut AnyObject = msg_send![settings_root_view_class(), alloc];
            let root: *mut AnyObject = msg_send![root, initWithFrame: host_bounds];
            let _: () = msg_send![root, setAutoresizingMask: 18u64];
            let _: () = msg_send![host, addSubview: root];
            // Keep one custom root under the system content host. It is the only layer that
            // paints the settings background and clips child surfaces to the window shape.
            // 在系统 content host 下保留一个自定义根视图，由它统一绘制背景并按窗口形状裁切子视图。
            release_obj(root);
            root
        } else {
            settings_root_view_for_host(host)
        };
        // The existing settings window should always retain the custom root. If AppKit replaced
        // the content host's children during a style transition, recreate it before rebuilding.
        // 复用窗口时系统可能在样式切换中替换 content host 的子视图；若根视图丢失则重新创建。
        let content: *mut AnyObject = if content.is_null() {
            let host_bounds: NSRect = msg_send![host, bounds];
            let root: *mut AnyObject = msg_send![settings_root_view_class(), alloc];
            let root: *mut AnyObject = msg_send![root, initWithFrame: host_bounds];
            let _: () = msg_send![root, setAutoresizingMask: 18u64];
            let _: () = msg_send![host, addSubview: root];
            release_obj(root);
            root
        } else {
            content
        };
        if existing_window.is_some() {
            // Remove only the old content hierarchy; keep the NSWindow, its frame, and key status
            // intact so a locale/theme refresh is an in-place redraw.
            // 只移除旧内容层级;保留 NSWindow、窗口位置和焦点状态,让语言/主题刷新成为原位重绘。
            //
            // 整棵旧层级即将销毁:先清空 tooltip 注册表(键是 view 地址)。残留的旧地址活到下一次
            // 点击时,新控件往往复用同一块内存——那时就是给已释放对象发消息(2026-09-15 22:19 的
            // SIGTRAP 崩溃正是这条路径:settings_window_send_event → tooltip 命中旧地址)。
            // The whole old hierarchy is about to be destroyed: clear the tooltip registries first
            // (their keys are view addresses). A stale address surviving into the next click is
            // usually reused by a new control -- i.e. a message to a freed object (exactly the
            // 2026-09-15 22:19 SIGTRAP crash path: settings_window_send_event -> tooltip hits a
            // stale address).
            tooltip::SettingsTooltip::clear_runtime_registries();
            let subviews: *mut AnyObject = msg_send![content, subviews];
            let count: usize = msg_send![subviews, count];
            for index in (0..count).rev() {
                let subview: *mut AnyObject = msg_send![subviews, objectAtIndex: index as isize];
                let _: () = msg_send![subview, removeFromSuperview];
            }
        }
        // content_h 只用于容器视图的满高尺寸(翻转 mask 后覆盖整个窗口);
        // 顶部锚定行的有效高度在翻转后用 layout_h 取(见下)。
        // content_h is only used for full-height containers (they cover the whole window after
        // the mask flip); top-anchored rows use layout_h measured after the flip (see below).
        let content_frame: NSRect = msg_send![content, frame];
        let content_h = content_frame.size.height;

        // 去掉红绿灯下方的标题栏分隔线:切到 fullSizeContentView + 透明标题栏后,
        // 内容区延伸到标题栏,AppKit 不再绘制那条 hairline;隐藏标题文字(系统设置同款观感)。
        // 注意:翻转 mask 前 contentView 可能尚未排版(未显示时返回全窗高度,实测 macOS 26
        // 上就是 690),所以不能在翻转前量有效高度。翻转后用 contentLayoutRect 量
        // 「红绿灯条带以下的内容可用区」(macOS 11+,部署目标 11.0 直接可用;min 兜底)。
        // 顶部锚定的行全部以 layout_h 定位,避免内容顶进红绿灯条带。
        // Remove the hairline under the traffic lights: with fullSizeContentView + a transparent
        // title bar the content extends into the title bar and AppKit stops drawing the separator;
        // the title text is hidden (System Settings look). The contentView must NOT be measured
        // before the flip -- an unlaid-out window reports the full height (690 on macOS 26 in
        // practice). After the flip, contentLayoutRect (macOS 11+; min target is 11.0) gives the
        // real layout area below the traffic-light strip; .min() guards a degenerate result.
        // All top-anchored rows are laid out against layout_h so nothing collides with the lights.
        let _: () = msg_send![window, setTitlebarAppearsTransparent: true];
        let _: () = msg_send![window, setStyleMask: style | (1 << 15)]; // NSWindowStyleMaskFullSizeContentView
        let _: () = msg_send![window, setTitleVisibility: 1isize]; // NSWindowTitleHidden
                                                                   // The scroll region spans the full window height (fullSizeContentView), so the content
                                                                   // scrolls all the way up behind the traffic-light strip. There is no bottom footer bar
                                                                   // anymore: each page embeds its own restore control at the end of its content.
        let page_viewport_h = content_h;

        // The root surface owns the window background and clipping. On macOS 27 it follows the
        // system's toolbar-window radius through container concentricity; older systems use the
        // measured fallback radius.
        // 根表面统一负责窗口背景与裁剪。macOS 27 通过容器同心圆角跟随工具栏窗口形状，旧系统使用固定回退值。
        apply_settings_root_surface(window, content, palette, window_clip_radius);

        let mut ui = SettingsUi {
            window,
            sidebar_general: std::ptr::null_mut(),
            sidebar_switcher: std::ptr::null_mut(),
            sidebar_mouse: std::ptr::null_mut(),
            sidebar_clipboard: std::ptr::null_mut(),
            sidebar_window_control: std::ptr::null_mut(),
            sidebar_quick_actions: std::ptr::null_mut(),
            sidebar_about: std::ptr::null_mut(),
            sidebar_highlight: std::ptr::null_mut(),
            general_view: std::ptr::null_mut(),
            switcher_view: std::ptr::null_mut(),
            mouse_view: std::ptr::null_mut(),
            clipboard_view: std::ptr::null_mut(),
            window_control_view: std::ptr::null_mut(),
            quick_actions_view: std::ptr::null_mut(),
            about_view: std::ptr::null_mut(),
            about_subtitle: std::ptr::null_mut(),
            theme: std::ptr::null_mut(),
            glass_style: std::ptr::null_mut(),
            glass_tint: std::ptr::null_mut(),
            glass_preview_switcher: std::ptr::null_mut(),
            glass_preview_clipboard: std::ptr::null_mut(),
            corner_radius: std::ptr::null_mut(),
            thumbnails_enabled: std::ptr::null_mut(),
            focused_thumbnail_prewarm: std::ptr::null_mut(),
            show_app_name_in_cards: std::ptr::null_mut(),
            card_text_size: std::ptr::null_mut(),
            card_text_size_value_label: std::ptr::null_mut(),
            status_bar_text_size: std::ptr::null_mut(),
            status_bar_text_size_value_label: std::ptr::null_mut(),
            modifier: std::ptr::null_mut(),
            locale: std::ptr::null_mut(),
            show_minimized: std::ptr::null_mut(),
            windows_enabled: std::ptr::null_mut(),
            overlay_position: std::ptr::null_mut(),
            activation_mode: std::ptr::null_mut(),
            log_level: std::ptr::null_mut(),
            launch_at_login: std::ptr::null_mut(),
            reverse_scroll: std::ptr::null_mut(),
            enable_mouse: std::ptr::null_mut(),
            scroll_mode: std::ptr::null_mut(),
            line_count: std::ptr::null_mut(),
            line_count_label: std::ptr::null_mut(),
            line_count_value_label: std::ptr::null_mut(),
            line_count_block: CollapsibleRows::empty(),
            disable_pointer_accel: std::ptr::null_mut(),
            pointer_accel_slider: std::ptr::null_mut(),
            pointer_accel_label: std::ptr::null_mut(),
            pointer_accel_value_label: std::ptr::null_mut(),
            pointer_accel_block: CollapsibleRows::empty(),
            thumbnail_only_block: CollapsibleRows::empty(),
            mapping_scroll: std::ptr::null_mut(),
            mapping_doc: std::ptr::null_mut(),
            mapping_card: std::ptr::null_mut(),
            mapping_panel: std::ptr::null_mut(),
            mapping_rows: Vec::new(),
            clipboard_enabled: std::ptr::null_mut(),
            window_control_enabled: std::ptr::null_mut(),
            window_control_up: std::ptr::null_mut(),
            window_control_down: std::ptr::null_mut(),
            window_control_left: std::ptr::null_mut(),
            window_control_right: std::ptr::null_mut(),
            window_control_display_up: std::ptr::null_mut(),
            window_control_display_down: std::ptr::null_mut(),
            window_control_display_left: std::ptr::null_mut(),
            window_control_display_right: std::ptr::null_mut(),
            quick_actions_enabled: std::ptr::null_mut(),
            quick_actions_open_settings: std::ptr::null_mut(),
            quick_actions_open_finder: std::ptr::null_mut(),
            quick_actions_show_desktop: std::ptr::null_mut(),
            quick_actions_lock_screen: std::ptr::null_mut(),
            quick_actions_locate_pointer: std::ptr::null_mut(),
            clipboard_persist: std::ptr::null_mut(),
            clipboard_move_used_to_top: std::ptr::null_mut(),
            clipboard_delete_after_paste: std::ptr::null_mut(),
            clipboard_clear_system_pasteboard_after_paste: std::ptr::null_mut(),
            clipboard_delete_block: CollapsibleRows::empty(),
            clipboard_max_entries: std::ptr::null_mut(),
            clipboard_auto_expire_days: std::ptr::null_mut(),
            clipboard_auto_expire_days_value_label: std::ptr::null_mut(),
            clipboard_show_source_app: std::ptr::null_mut(),
            clipboard_pin_follow: std::ptr::null_mut(),
            add_mapping_button: std::ptr::null_mut(),
            mapping_enabled: std::ptr::null_mut(),
            mapping_empty: std::ptr::null_mut(),
            device_indicator: std::ptr::null_mut(),
            restore_defaults: RestoreDefaultsControl::empty(),
            page_restores: std::array::from_fn(|_| RestoreDefaultsControl::empty()),
            accessibility_warning_view: std::ptr::null_mut(),
            update_auto_check: std::ptr::null_mut(),
            update_auto_download: std::ptr::null_mut(),
            update_check_button: std::ptr::null_mut(),
            update_host: std::ptr::null_mut(),
            update_host_window: std::ptr::null_mut(),
            update_card: std::ptr::null_mut(),
            update_card_shadow: std::ptr::null_mut(),
            update_divider: std::ptr::null_mut(),
            update_card_compact_h: 0.0,
            update_card_expanded: false,
            update_host_origin_y: 0.0,
        };

        // The sidebar and detail pane meet directly at the original sidebar boundary; their
        // backgrounds provide the visual split instead of an inset outer card.
        // 左侧导航和右侧详情直接衔接，通过两种背景色区分，不再使用内缩的外框卡片。
        let content_x = card_w;
        let detail_w = view_w - content_x;
        let page_inset = 32.0;
        let page_x = content_x + page_inset;
        let content_w = detail_w - page_inset * 2.0;
        let layout = SettingsLayout::new(content_w);
        let label_x = layout.label_x;
        let label_w = layout.label_w;
        let ctrl_w = layout.control_w;
        let ctrl_x = layout.control_x;
        // HTML `.row` uses a 34pt control. All card rows now use a compact 54pt rhythm; detailed
        // explanatory paragraphs are intentionally omitted from the card interior.
        // HTML `.row` 控件仍为 34pt；卡片内统一使用紧凑的 54pt 节奏，详细说明不再塞进行内。
        let row_h = layout.row_h;
        let described_row_h = layout.described_row_h;

        let target = match *MENU_TARGET.lock().unwrap() {
            Some(t) => t.0,
            None => return,
        };

        // --- 侧边栏 sidebar(悬浮玻璃卡片,系统设置同款观感)---
        // macOS 26+ 用 NSGlassEffectView(Liquid Glass,不设 tint 用系统默认);
        // 旧版用 NSVisualEffectView + sidebar 材质(经典磨砂侧边栏)。
        // --- Sidebar: a flat navigation column, matching the reference layout ---
        // macOS 26+ uses NSGlassEffectView (Liquid Glass, system default tint);
        // older macOS uses NSVisualEffectView with the sidebar material (classic frosted look).
        // The glass material supplies the subtle separation from the content pane.
        let card_h = content_h - card_margin * 2.0;
        let sidebar_content: *mut AnyObject;
        let sidebar_view: *mut AnyObject = if AnyClass::get(c"NSGlassEffectView").is_some() {
            let cls = AnyClass::get(c"NSGlassEffectView").unwrap();
            let g: *mut AnyObject = msg_send![cls, alloc];
            let g: *mut AnyObject = msg_send![g, initWithFrame: NSRect::new(NSPoint::new(card_margin, card_margin), NSSize::new(card_w, card_h))];
            let _: () = msg_send![g, setStyle: 0i64]; // NSGlassEffectViewStyleRegular
            let _: () = msg_send![g, setCornerRadius: card_radius];
            // AppKit only guarantees Liquid Glass composition for the assigned contentView.
            // NSGlassEffectView 的玻璃合成只保证作用于显式设置的 contentView。
            let inner: *mut AnyObject = msg_send![class!(NSView), alloc];
            let inner: *mut AnyObject = msg_send![
                inner,
                initWithFrame: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(card_w, card_h)
                )
            ];
            let _: () = msg_send![inner, setAutoresizingMask: 18u64];
            let _: () = msg_send![g, setContentView: inner];
            sidebar_content = inner;
            g
        } else {
            let ve: *mut AnyObject = msg_send![class!(NSVisualEffectView), alloc];
            let ve: *mut AnyObject = msg_send![ve, initWithFrame: NSRect::new(NSPoint::new(card_margin, card_margin), NSSize::new(card_w, card_h))];
            let _: () = msg_send![ve, setMaterial: 8u64]; // NSVisualEffectMaterialSidebar
            let _: () = msg_send![ve, setBlendingMode: 0u64]; // BehindWindow
            let _: () = msg_send![ve, setState: 1u64]; // Active
            let _: () = msg_send![ve, setWantsLayer: true];
            let ve_layer: *mut AnyObject = msg_send![ve, layer];
            if !ve_layer.is_null() {
                let _: () = msg_send![ve_layer, setCornerRadius: card_radius];
                let _: () = msg_send![ve_layer, setMasksToBounds: true];
            }
            sidebar_content = ve;
            ve
        };
        // Keep the navigation pane a distinct light-gray surface, while the detail pane uses the
        // window background. This mirrors the HTML reference's two-pane split without an inset
        // border around the whole settings area.
        let sidebar_layer: *mut AnyObject = msg_send![sidebar_view, layer];
        if !sidebar_layer.is_null() {
            layer_set_background(
                sidebar_layer,
                crate::ffi::hex_to_cg_color(palette.sidebar_bg),
            );
        }
        // 自适应:左侧锚定、高度随窗口拉伸(HeightSizable|MaxXMargin = 16|4 = 20)。
        // Adaptive: left-anchored, height stretches with the window.
        let _: () = msg_send![sidebar_view, setAutoresizingMask: 20u64];
        let _: () = msg_send![content, addSubview: sidebar_view];
        release_obj(sidebar_view);

        // HTML `.sidebar { border-right: 1px solid rgba(0,0,0,.055) }`.
        let sidebar_divider: *mut AnyObject = msg_send![class!(NSView), alloc];
        let sidebar_divider: *mut AnyObject = msg_send![
            sidebar_divider,
            initWithFrame: NSRect::new(
                NSPoint::new(card_w - 1.0, 0.0),
                NSSize::new(1.0, content_h)
            )
        ];
        let _: () = msg_send![sidebar_divider, setWantsLayer: true];
        let divider_layer: *mut AnyObject = msg_send![sidebar_divider, layer];
        if !divider_layer.is_null() {
            layer_set_background(
                divider_layer,
                crate::ffi::hex_to_cg_color(palette.separator),
            );
        }
        let _: () = msg_send![sidebar_divider, setAutoresizingMask: 20u64];
        let _: () = msg_send![content, addSubview: sidebar_divider];
        release_obj(sidebar_divider);

        // The right detail pane has its own white surface, directly beside the gray sidebar.
        // The custom class adds the HTML `.main` radial highlight (82% 0%) over the flat fill.
        // 右侧详情区自有浅色表面,紧邻灰色侧栏。自定义类在纯色填充之上叠加
        // HTML `.main` 的径向高光(82% 0%)。
        let main_background: *mut AnyObject =
            msg_send![widgets::settings_pane_highlight_view_class(), alloc];
        let main_background: *mut AnyObject = msg_send![
            main_background,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(view_w, content_h)
            )
        ];
        let _: () = msg_send![main_background, setWantsLayer: true];
        let main_layer: *mut AnyObject = msg_send![main_background, layer];
        if !main_layer.is_null() {
            layer_set_background(main_layer, crate::ffi::hex_to_cg_color(palette.detail_bg));
        }
        let _: () = msg_send![main_background, setAutoresizingMask: 18u64];
        let _: () = msg_send![
            content,
            addSubview: main_background,
            positioned: -1isize,
            relativeTo: sidebar_view
        ];
        release_obj(main_background);

        // Sidebar identity block, matching the redesign's app title and subtitle above the nav.
        let app_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let app_title: *mut AnyObject = msg_send![
            app_title,
            initWithFrame: NSRect::new(
                // Sidebar content spans the full content view, including the unified toolbar
                // strip where the traffic lights live. Anchor the identity block to that full
                // height so it follows the HTML sidebar's compact top padding instead of being
                // pushed down by the toolbar's contentLayoutRect inset.
                // Title 20pt/700 matches the HTML `.brand-title` (font-size:20px; weight:700).
                NSPoint::new(24.0, content_h - 78.0),
                NSSize::new(card_w - 48.0, 26.0)
            )
        ];
        set_field(app_title, "Oh My Tab");
        let _: () = msg_send![app_title, setBezeled: false];
        let _: () = msg_send![app_title, setDrawsBackground: false];
        let _: () = msg_send![app_title, setEditable: false];
        let app_title_font: *mut AnyObject =
            msg_send![class!(NSFont), boldSystemFontOfSize: 20.0f64];
        let _: () = msg_send![app_title, setFont: app_title_font];
        let app_title_color = settings_text_color(SettingsTextRole::Primary);
        let _: () = msg_send![app_title, setTextColor: app_title_color];
        // 贴顶、贴左:窗口高度可调,身份区必须跟随红绿灯条带而不是漂向底部。
        // Top- and left-anchored: the window height is adjustable, so the identity block must
        // follow the traffic-light strip instead of drifting downward.
        let _: () = msg_send![app_title, setAutoresizingMask: 12u64];
        let _: () = msg_send![sidebar_content, addSubview: app_title];
        release_obj(app_title);
        let app_subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let app_subtitle: *mut AnyObject = msg_send![
            app_subtitle,
            initWithFrame: NSRect::new(
                NSPoint::new(24.0, content_h - 102.0),
                NSSize::new(card_w - 48.0, 18.0)
            )
        ];
        set_field(app_subtitle, t("settings.window_title"));
        let _: () = msg_send![app_subtitle, setBezeled: false];
        let _: () = msg_send![app_subtitle, setDrawsBackground: false];
        let _: () = msg_send![app_subtitle, setEditable: false];
        let app_subtitle_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
        let _: () = msg_send![app_subtitle, setFont: app_subtitle_font];
        let app_subtitle_color = settings_text_color(SettingsTextRole::Muted);
        let _: () = msg_send![app_subtitle, setTextColor: app_subtitle_color];
        let _: () = msg_send![app_subtitle, setAutoresizingMask: 12u64];
        let _: () = msg_send![sidebar_content, addSubview: app_subtitle];
        release_obj(app_subtitle);

        // 侧边栏选中行的高亮背景(layer-backed NSView,theme 感知色),先于按钮加入以便按钮文字叠在上层。
        // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
        // added before the buttons so button titles draw on top of it.
        // 卡片内布局:内边距 12;按钮顶边按完整侧边栏高度定位,靠近红绿灯
        // (btn_y0 为卡片坐标系)。
        // Card-local layout: 12pt inner margins. The buttons stay close to the traffic lights;
        // btn_y0 is anchored to the full sidebar height rather than the toolbar-inset height.
        let btn_w = card_w - 28.0;
        let btn_h = SettingsSidebar::row_height(btn_w);
        // Sidebar navigation is also anchored to the full-height sidebar. Using layout_h here
        // includes the toolbar inset a second time and leaves a large blank gap above the title.
        let btn_y0 = content_h - card_margin - 112.0 - btn_h;
        let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
        let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(14.0, btn_y0), NSSize::new(btn_w, btn_h))];
        let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // 贴顶、贴左 / top- and left-anchored
        let _: () = msg_send![highlight, setWantsLayer: true];
        let hl_layer: *mut AnyObject = msg_send![highlight, layer];
        let _: () = msg_send![hl_layer, setCornerRadius: 10.0f64];
        // 选中高亮用系统强调色(controlAccentColor),与 NSSwitch 开启的蓝色一致
        // (LinearMouse 侧边栏选中高亮同款)。
        // Selection highlight uses the system accent color (controlAccentColor), matching the
        // NSSwitch's on-state blue (same as LinearMouse's sidebar selection highlight).
        // The redesign uses a soft accent wash for the active row rather than a solid blue fill.
        layer_set_background(hl_layer, crate::ffi::hex_to_cg_color(palette.selection_bg));
        let _: () = msg_send![sidebar_content, addSubview: highlight];
        release_obj(highlight);
        ui.sidebar_highlight = highlight;

        // Seven sidebar buttons (borderless, tags 0..6; click triggers handleSettingsSidebar:).
        let sidebar_buttons =
            SettingsSidebar::build(sidebar_content, target, 14.0, btn_y0, btn_w, btn_h);
        [
            &mut ui.sidebar_general,
            &mut ui.sidebar_switcher,
            &mut ui.sidebar_mouse,
            &mut ui.sidebar_clipboard,
            &mut ui.sidebar_window_control,
            &mut ui.sidebar_quick_actions,
            &mut ui.sidebar_about,
        ]
        .iter_mut()
        .zip(sidebar_buttons)
        .for_each(|(slot, button)| **slot = button);
        widgets::set_sidebar_update_indicator(
            ui.sidebar_about,
            UPDATE_AVAILABLE.load(Ordering::SeqCst),
        );

        // HTML `.sidebar-footer`: the complete restore control is one semantic component, with
        // its separator and morphing confirm/cancel rows owned together.
        // HTML `.sidebar-footer`:整个恢复控件作为一个语义组件，统一管理分割线和 morph 确认/取消行。
        ui.restore_defaults = RestoreDefaultsControl::build(sidebar_content, target, card_w);

        // The scroll view spans from the left gutter to the detail pane's right edge, so the
        // overlay scrollbar sits flush with the window edge (matching the OK/Cancel footer) and
        // no longer floats 32pt in from the right. The content keeps its 32pt gutter margins
        // inside the document, so only the scrollbar's position changes.
        let page_frame = NSRect::new(
            NSPoint::new(page_x, 0.0),
            NSSize::new(detail_w - page_inset, page_viewport_h),
        );
        // Build from generous provisional heights. They are intentionally not shrunk after child
        // frames are assigned: the pages use manual top-anchored coordinates, so post-hoc
        // shrinking would let AppKit move the children a second time.
        // 先用宽松的临时高度构建；子视图定位后不再收缩 document，因为页面是手动顶部锚定坐标，
        // 布局后收缩会让 AppKit 再次移动子视图。
        // 各页高度含 SettingsPageHeader 的 42pt 顶部留白(较旧版 24pt 多 18)。
        // Built from generous provisional heights. They are intentionally not shrunk after child
        // frames are assigned: the pages use manual top-anchored coordinates, so post-hoc
        // shrinking would let AppKit move the children a second time. Every height includes
        // SettingsPageHeader's 42pt top padding (18 more than the old 24pt inset).
        let general_doc_h = 1138.0;
        let switcher_doc_h = 1432.0;
        let mouse_doc_h = 1620.0;
        let clipboard_doc_h = 978.0;
        // 窗口控制页包含总开关、四个方向开关和四个跨显示器开关。
        // The window-control page contains the master, four direction switches, and four
        // cross-display switches.
        let window_control_doc_h = 1102.0;
        // 快捷操作页:总开关 + 五个动作开关,结构与窗口控制页一致。
        // Quick-actions page: master plus five action switches, mirroring the window-control
        // page.
        let quick_actions_doc_h = 854.0;
        let about_doc_h = 1300.0;

        let general_page = SettingsPage::new(content, page_frame, general_doc_h, false);
        let general_root = general_page.scroll;
        let general_view = general_page.document;
        ui.general_view = general_root;
        let switcher_page = SettingsPage::new(content, page_frame, switcher_doc_h, true);
        let switcher_root = switcher_page.scroll;
        let switcher_view = switcher_page.document;
        ui.switcher_view = switcher_root;
        let mouse_page = SettingsPage::new(content, page_frame, mouse_doc_h, true);
        let mouse_root = mouse_page.scroll;
        let mouse_view = mouse_page.document;
        ui.mouse_view = mouse_root;
        let clipboard_page = SettingsPage::new(content, page_frame, clipboard_doc_h, true);
        let clipboard_root = clipboard_page.scroll;
        let clipboard_view = clipboard_page.document;
        ui.clipboard_view = clipboard_root;
        let window_control_page =
            SettingsPage::new(content, page_frame, window_control_doc_h, true);
        let window_control_root = window_control_page.scroll;
        let window_control_view = window_control_page.document;
        ui.window_control_view = window_control_root;
        let quick_actions_page = SettingsPage::new(content, page_frame, quick_actions_doc_h, true);
        let quick_actions_root = quick_actions_page.scroll;
        let quick_actions_view = quick_actions_page.document;
        ui.quick_actions_view = quick_actions_root;
        let about_page = SettingsPage::new(content, page_frame, about_doc_h, true);
        let about_root = about_page.scroll;
        let about_view = about_page.document;
        ui.about_view = about_root;

        // ===== 通用页内容 general page content =====
        let general_top = general_doc_h - 24.0;
        let mut y = general_doc_h; // top cursor: bottom edge of the next element
        let general_title_h = SettingsPageHeader::attach(
            general_view,
            &t("settings.sidebar_general"),
            6.0,
            general_doc_h,
            content_w - 12.0,
        );
        y -= general_title_h + 18.0;

        // --- Accessibility 权限警告条(通用页顶部覆盖;仅缺权限时显示,show_settings 里按 setHidden 切换) ---
        // --- Accessibility permission warning banner (floats at the top of General; shown only
        //  when permission is missing, toggled via setHidden in show_settings) ---
        // banner 不占用布局空间(通用页内容紧贴顶部),而是在内容构建完后作为最后一个
        // subview 添加,覆盖在顶部。frame 固定定位,不随 y 布局游标变化。
        // The banner does not reserve layout space (General content starts at the top); it is
        // added as the last subview after the content, floating over the top. Its frame is fixed
        // and independent of the y layout cursor.
        let banner_h = 48.0;
        let banner: *mut AnyObject = msg_send![class!(NSView), alloc];
        let banner: *mut AnyObject = msg_send![
            banner,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, general_top - banner_h),
                NSSize::new(content_w, banner_h)
            )
        ];
        // 自适应:宽度拉伸、顶部锚定(WidthSizable|MinYMargin = 10)。
        // 注意:这里不 addSubview;在通用页内容构建完后统一添加(保证在最上层)。
        // Note: not added here; added after the General content build so it stays on top.
        let _: () = msg_send![banner, setAutoresizingMask: 10u64];
        ui.accessibility_warning_view = banner;

        // 警告文字:多行换行,系统红色 / warning text: word-wrapped, system red
        let warning_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let warning_label: *mut AnyObject = msg_send![
            warning_label,
            initWithFrame: NSRect::new(
                NSPoint::new(12.0, 6.0),
                NSSize::new(content_w - 160.0, banner_h - 12.0)
            )
        ];
        let wl = make_nsstring(&t("settings.accessibility_warning"));
        let _: () = msg_send![warning_label, setStringValue: wl];
        CFRelease(wl as *const c_void);
        let _: () = msg_send![warning_label, setEditable: false];
        let _: () = msg_send![warning_label, setBezeled: false];
        let _: () = msg_send![warning_label, setDrawsBackground: false];
        let _: () = msg_send![warning_label, setUsesSingleLineMode: false];
        let _: () = msg_send![warning_label, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
        let red: *mut AnyObject = msg_send![class!(NSColor), systemRedColor];
        let _: () = msg_send![warning_label, setTextColor: red];
        // 自适应:宽度随 banner 拉伸、左锚定(WidthSizable = 2)。
        let _: () = msg_send![warning_label, setAutoresizingMask: 2u64];
        let _: () = msg_send![banner, addSubview: warning_label];
        release_obj(warning_label);

        // 「打开隐私与安全性」按钮 / "Open Privacy & Security" button
        let open_btn = SettingsButton::action(
            NSRect::new(
                NSPoint::new(content_w - 150.0, (banner_h - 28.0) / 2.0),
                NSSize::new(140.0, 28.0),
            ),
            &t("settings.btn_open_privacy"),
            target,
            sel!(handleOpenPrivacy:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![banner, addSubview: open_btn];
        release_obj(open_btn);

        // 默认按当前权限显隐(有权限就隐藏)/ initial visibility: hidden when permission is already granted
        let _: () = msg_send![banner, setHidden: has_accessibility_permission()];

        // --- 外观 Appearance ---
        y -= 12.0;
        let appearance_header_y = y;
        let theme_items = [
            t("settings.theme_dark"),
            t("settings.theme_light"),
            t("settings.theme_auto"),
        ];
        let theme_item_refs: Vec<&str> = theme_items.iter().map(String::as_str).collect();
        let theme_metrics =
            SettingsSelect::metrics(ctrl_w, &theme_item_refs, row_h, described_row_h);
        y = layout.next_row_cursor(y, theme_metrics.row_h);
        ui.theme = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            theme_metrics.row_h,
            &t("settings.row_theme"),
            &t("settings.desc_theme"),
            SettingsControl::popup(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                theme_metrics.control_h,
                &theme_item_refs,
                0,
            ),
        );
        bind_control(target, ui.theme);
        y -= theme_metrics.row_h;
        SettingsRow::separator(general_view, y + theme_metrics.row_h, content_w);
        let glass_style_metrics =
            SettingsSelect::metrics(ctrl_w, &["Regular", "Clear"], row_h, described_row_h);
        ui.glass_style = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            glass_style_metrics.row_h,
            &t("settings.row_glass_style"),
            &t("settings.desc_glass_style"),
            SettingsControl::popup(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                glass_style_metrics.control_h,
                &["Regular", "Clear"],
                0,
            ),
        );
        bind_control(target, ui.glass_style);
        y -= glass_style_metrics.row_h;
        SettingsRow::separator(general_view, y + glass_style_metrics.row_h, content_w);
        ui.glass_tint = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_glass_tint"),
            &t("settings.desc_glass_tint"),
            make_color_well(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                row_h,
                &Config::default().appearance.glass_tint,
                target,
            ),
        );
        configure_glass_tint_panel(target);
        let appearance_card_bottom = layout.card_bottom(y);
        let appearance_card_top = layout.card_top(appearance_header_y);
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, appearance_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    appearance_card_top - appearance_card_bottom,
                ),
            ),
            &t("settings.header_appearance"),
        );

        // --- 实时预览 Live preview ---
        y = layout.next_section_cursor(y);
        let preview_header_y = y;
        y = layout.next_row_cursor(y, row_h);
        let preview_h = 90.0;
        let preview_y = y - preview_h;
        let preview_w = (content_w - 2.0 * label_x - 12.0) / 2.0;
        let right_preview_x = label_x + preview_w + 12.0;
        add_preview_caption(
            general_view,
            &t("settings.preview_switcher"),
            label_x,
            preview_y + preview_h + 3.0,
            preview_w,
        );
        add_preview_caption(
            general_view,
            &t("settings.preview_clipboard"),
            right_preview_x,
            preview_y + preview_h + 3.0,
            preview_w,
        );
        ui.glass_preview_switcher =
            make_glass_preview(general_view, label_x, preview_y, preview_w, preview_h, true);
        ui.glass_preview_clipboard = make_glass_preview(
            general_view,
            right_preview_x,
            preview_y,
            preview_w,
            preview_h,
            false,
        );
        y = preview_y;
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, preview_y - 12.0),
                NSSize::new(
                    content_w - 12.0,
                    (preview_header_y - layout.card_header_gap) - (preview_y - 12.0),
                ),
            ),
            &t("settings.header_preview"),
        );

        // --- 语言 Language ---
        y = layout.next_section_cursor(y);
        let language_header_y = y;
        let locale_metrics =
            SettingsSelect::metrics(ctrl_w, &LOCALE_LABELS, row_h, described_row_h);
        y = layout.next_row_cursor(y, locale_metrics.row_h);
        let language_card_bottom = layout.card_bottom(y);
        let language_card_top = layout.card_top(language_header_y);
        ui.locale = SettingsRow::plain(
            general_view,
            label_x,
            y,
            label_w,
            locale_metrics.row_h,
            &t("settings.row_locale"),
            SettingsControl::popup(
                ctrl_x,
                y,
                ctrl_w,
                locale_metrics.control_h,
                &LOCALE_LABELS,
                0,
            ),
        );
        bind_control(target, ui.locale);
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, language_card_bottom),
                NSSize::new(content_w - 12.0, language_card_top - language_card_bottom),
            ),
            &t("settings.header_language"),
        );

        // --- 日志 Logging ---
        y = layout.next_section_cursor(y);
        let logging_header_y = y;
        // 日志级别下拉框:项 = [debug, info];默认 index 1(info)。
        // Log level popup: items = [debug, info]; default index 1 (info).
        let log_levels: [&str; 2] = ["Debug", "Info"];
        let log_level_metrics =
            SettingsSelect::metrics(ctrl_w, &log_levels, row_h, described_row_h);
        y = layout.next_row_cursor(y, log_level_metrics.row_h);
        ui.log_level = SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            log_level_metrics.row_h,
            &t("settings.row_log_level"),
            &t("settings.desc_log_level"),
            SettingsControl::popup(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                log_level_metrics.control_h,
                &log_levels,
                1,
            ),
        );
        bind_control(target, ui.log_level);
        // 导出日志:左标题+说明、右操作按钮(与日志级别同一张卡片;按钮不参与
        // ControlField 即时生效调度,直接走 target/action)。
        // Export logs: title+description on the left, action button on the right (same card
        // as the log level; the button opts out of ControlField live-apply and goes straight
        // through target/action).
        y = layout.next_row_cursor(y, described_row_h);
        // 卡片内部分割线:线下方就是本导出行(separator_above_row 收相对行算术)。
        // In-card divider: the export row sits right below it (separator_above_row owns the
        // row-relative math).
        SettingsRow::separator_above_row(general_view, y, described_row_h, content_w);
        const EXPORT_BTN_W: f64 = 110.0;
        let export_btn = SettingsControl::button(
            ctrl_x + ctrl_w - EXPORT_BTN_W,
            y,
            EXPORT_BTN_W,
            28.0,
            &t("settings.btn_export_logs"),
            target,
            sel!(handleExportLogs:),
            SettingsButtonRole::Action,
        );
        SettingsRow::described(
            general_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_export_logs"),
            &t("settings.desc_export_logs"),
            export_btn,
        );
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(logging_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_logging"),
        );

        // --- 启动 Startup ---
        y = layout.next_section_cursor(y);
        let startup_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // 开机自启开关:标题留空(左侧 row label 已说明),仅放一个 switch。
        // Launch-at-login switch: no title (the row label on the left already describes it).
        ui.launch_at_login = SettingsRow::described(
            general_view,
            label_x,
            y,
            content_w - label_x * 2.0 - 58.0,
            described_row_h,
            &t("settings.row_launch_at_login"),
            &t("settings.desc_launch_at_login"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        bind_control(target, ui.launch_at_login);
        SettingsSection::attach(
            general_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(startup_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_startup"),
        );
        let general_content_bottom = layout.card_bottom(y);

        // ===== 应用切换浮窗页内容 switcher overlay page content =====
        let mut y = switcher_doc_h;
        let switcher_title_h = SettingsPageHeader::attach(
            switcher_view,
            &t("settings.sidebar_switcher"),
            6.0,
            switcher_doc_h,
            content_w - 12.0,
        );
        y -= switcher_title_h + 18.0;

        // --- 窗口 Window ---
        y -= 12.0;
        let windows_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // 窗口切换总开关:关闭后 Cmd+Tab 透传给系统(原生切换器接管)。
        // App-switcher master switch: off = Cmd+Tab passes through to the system.
        let windows_master_row_y = y;
        ui.windows_enabled = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_windows_enabled"),
            &t("settings.desc_windows_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        let _: () = msg_send![ui.windows_enabled, setTarget: target];
        let _: () = msg_send![
            ui.windows_enabled,
            setAction: sel!(handleWindowsEnabledToggle:)
        ];
        SettingsSection::attach(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(windows_master_row_y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(windows_header_y) - layout.card_bottom(windows_master_row_y),
                ),
            ),
            &t("settings.header_windows"),
        );
        // The remaining window settings form a second card with its own section title.
        // 其余窗口设置单独成卡,并为卡片补充独立的小标题。
        y = layout.next_section_cursor(y);
        let windows_options_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // show_minimized 开关(切换器语义本就只有显/隐两态,用 Toggle 比下拉更直观)。
        // 英文标签较长,该行标签加宽;开关保留参考页面的右侧内边距。
        // show_minimized is inherently two-state, so a toggle is clearer than a popup. The long
        // English label uses a wider label column, while the switch stays aligned to the popups.
        ui.show_minimized = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            220.0,
            &t("settings.row_show_minimized"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        )
        .1;
        bind_control(target, ui.show_minimized);
        // 窗口显示模式:仅图标或图标和缩略图;配置仍由 thumbnails_enabled 布尔值保存。
        // Window display mode: icons only or icons and thumbnails; the config remains stored as
        // the thumbnails_enabled boolean.
        let window_display_mode_labels = [
            t("settings.window_display_mode_icons"),
            t("settings.window_display_mode_icons_thumbnails"),
        ];
        let window_display_mode_refs: Vec<&str> = window_display_mode_labels
            .iter()
            .map(|s| s.as_str())
            .collect();
        let display_mode_metrics =
            SettingsSelect::metrics(ctrl_w, &window_display_mode_refs, row_h, described_row_h);
        // The preceding show-minimized row uses the shared described-row height; do not let
        // the next popup's measured height move its separator.
        // 前一行“显示最小化窗口”使用统一 described 行高；不能让下一行下拉框的动态高度
        // 改变这一行的游标和分隔线位置。
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
        ui.thumbnails_enabled = SettingsRow::tall_with_height(
            switcher_view,
            label_x,
            y,
            220.0,
            display_mode_metrics.row_h,
            &t("settings.row_window_display_mode"),
            SettingsControl::popup(
                ctrl_x,
                y + 10.0,
                ctrl_w,
                display_mode_metrics.control_h,
                &window_display_mode_refs,
                0,
            ),
        )
        .1;
        bind_control(target, ui.thumbnails_enabled);
        y = layout.next_row_cursor(y, display_mode_metrics.row_h);
        // The popup row may be taller than the standard described row in long locales.
        // 下拉行在长文案语言下可能高于标准 described 行，分隔线必须复用实际行高。
        let prewarm_separator = SettingsRow::separator_above_row(
            switcher_view,
            y,
            display_mode_metrics.row_h,
            content_w,
        );
        // 这两行只在"图标和缩略图"模式下有意义:纯图标模式没有缩略图可预热,应用名也本就
        // 单独一行显示(见下面注释),因此整块随显示模式显隐。
        // These two rows only mean something in icons-and-thumbnails mode: there is no thumbnail
        // to prewarm in icon-only mode, and the app name already gets its own line there (see
        // below), so the whole block follows the display mode.
        let (prewarm_label, prewarm_switch) = SettingsRow::tall_with_height(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_focused_thumbnail_prewarm"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        ui.focused_thumbnail_prewarm = prewarm_switch;
        bind_control(target, ui.focused_thumbnail_prewarm);
        // 卡片标题中的应用名:开关决定缩略图卡片标题行是否在窗口标题前显示应用名,
        // 两者以 " · " 分隔;纯图标模式的应用名本就在标题下方单独一行,不受该开关影响。
        // App name in card titles: the switch controls whether the thumbnail card's caption
        // shows the app name before the window title, separated by " · "; icon-only mode
        // already shows the app name on its own line below the title, so it is unaffected.
        y = layout.next_row_cursor(y, described_row_h);
        let app_name_separator =
            SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
        let (app_name_label, app_name_switch) = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            220.0,
            &t("settings.row_show_app_name_in_cards"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        ui.show_app_name_in_cards = app_name_switch;
        bind_control(target, ui.show_app_name_in_cards);
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
        ui.card_text_size = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_card_text_size"),
            &t("settings.desc_card_text_size"),
            SettingsControl::slider(
                ctrl_x,
                y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                TEXT_SIZE_MIN,
                TEXT_SIZE_MAX,
                TEXT_SIZE_DEFAULT,
                // 双击恢复默认字号(15pt)。
                // Double-click restores the default size (15pt).
                Some(TEXT_SIZE_DEFAULT as f64),
            ),
        );
        ui.card_text_size_value_label =
            SettingsRow::attach_slider_readout(switcher_view, ui.card_text_size, TEXT_SIZE_DEFAULT);
        bind_control(target, ui.card_text_size);
        y = layout.next_row_cursor(y, described_row_h);
        SettingsRow::separator_above_row(switcher_view, y, described_row_h, content_w);
        ui.status_bar_text_size = SettingsRow::described(
            switcher_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_status_bar_text_size"),
            &t("settings.desc_status_bar_text_size"),
            SettingsControl::slider(
                ctrl_x,
                y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                TEXT_SIZE_MIN,
                TEXT_SIZE_MAX,
                TEXT_SIZE_DEFAULT,
                // 双击恢复默认字号(15pt)。
                // Double-click restores the default size (15pt).
                Some(TEXT_SIZE_DEFAULT as f64),
            ),
        );
        ui.status_bar_text_size_value_label = SettingsRow::attach_slider_readout(
            switcher_view,
            ui.status_bar_text_size,
            TEXT_SIZE_DEFAULT,
        );
        bind_control(target, ui.status_bar_text_size);
        // overlay_position 下拉框:项 = [跟随激活窗口, 始终显示在主屏幕];默认 index 0。
        // overlay_position popup: [Follow Active Window, Always on Main Screen]; default index 0.
        let op_labels = [
            t("settings.overlay_position_follow_active"),
            t("settings.overlay_position_main_screen"),
        ];
        let op_label_refs: Vec<&str> = op_labels.iter().map(|s| s.as_str()).collect();
        let op_metrics = SettingsSelect::metrics(ctrl_w, &op_label_refs, row_h, described_row_h);
        y = layout.next_row_cursor(y, op_metrics.row_h);
        SettingsRow::separator_above_row(switcher_view, y, op_metrics.row_h, content_w);
        ui.overlay_position = SettingsRow::tall_with_height(
            switcher_view,
            label_x,
            y,
            label_w,
            op_metrics.row_h,
            &t("settings.row_overlay_position"),
            SettingsControl::popup(
                ctrl_x,
                y + (op_metrics.row_h - op_metrics.control_h) / 2.0,
                ctrl_w,
                op_metrics.control_h,
                &op_label_refs,
                0,
            ),
        )
        .1;
        bind_control(target, ui.overlay_position);
        // 窗口激活方式下拉框: index 0 = 鼠标悬停时激活, 1 = 点击窗口时激活;默认 index 0。
        // Window activation mode popup: index 0 = activate on hover, 1 = activate on click;
        // default index 0.
        let activation_labels = [
            t("settings.activation_mode_hover"),
            t("settings.activation_mode_click"),
        ];
        let activation_label_refs: Vec<&str> =
            activation_labels.iter().map(|s| s.as_str()).collect();
        let activation_metrics =
            SettingsSelect::metrics(ctrl_w, &activation_label_refs, row_h, described_row_h);
        y = layout.next_row_cursor(y, activation_metrics.row_h);
        SettingsRow::separator_above_row(switcher_view, y, activation_metrics.row_h, content_w);
        ui.activation_mode = SettingsRow::tall_with_height(
            switcher_view,
            label_x,
            y,
            label_w,
            activation_metrics.row_h,
            &t("settings.row_activation_mode"),
            SettingsControl::popup(
                ctrl_x,
                y + (activation_metrics.row_h - activation_metrics.control_h) / 2.0,
                ctrl_w,
                activation_metrics.control_h,
                &activation_label_refs,
                0,
            ),
        )
        .1;
        bind_control(target, ui.activation_mode);
        y = layout.next_row_cursor(y, activation_metrics.row_h);
        SettingsRow::separator_above_row(switcher_view, y, activation_metrics.row_h, content_w);
        ui.corner_radius = SettingsRow::tall(
            switcher_view,
            label_x,
            y,
            label_w,
            &t("settings.row_corner_radius"),
            SettingsControl::text_input(ctrl_x, y + 10.0, ctrl_w, row_h, "64"),
        )
        .1;
        let options_card_parts = SettingsSection::attach(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(windows_options_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_window_options"),
        );
        // 仅缩略图模式的两行:整块两行高(每行 row_gap + described_row_h),连同各自上方的
        // 分割线一起显隐。
        // The thumbnail-only pair: a block two rows tall (row_gap + described_row_h each), with
        // each row's own divider going along with it.
        ui.thumbnail_only_block = CollapsibleRows::new(
            options_card_parts.card,
            options_card_parts.shadow,
            vec![
                prewarm_label,
                prewarm_switch,
                app_name_label,
                app_name_switch,
            ],
            vec![prewarm_separator, app_name_separator],
            2.0 * (layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H),
        );

        // --- 键盘 Keyboard ---
        y = layout.next_section_cursor(y);
        let keyboard_header_y = y;
        // 修饰键下拉项:显示 Option+Tab / Command+Tab;值由索引映射到 option/command。
        // Modifier popup shows Option+Tab / Command+Tab; the index maps to option/command.
        let mod_labels = [
            t("settings.modifier_option"),
            t("settings.modifier_command"),
        ];
        let mod_label_refs: Vec<&str> = mod_labels.iter().map(|s| s.as_str()).collect();
        let mod_metrics = SettingsSelect::metrics(ctrl_w, &mod_label_refs, row_h, described_row_h);
        y = layout.next_row_cursor(y, mod_metrics.row_h);
        let keyboard_card_bottom = layout.card_bottom(y);
        let keyboard_card_top = layout.card_top(keyboard_header_y);
        ui.modifier = SettingsRow::tall_with_height(
            switcher_view,
            label_x,
            y,
            label_w,
            mod_metrics.row_h,
            &t("settings.row_modifier"),
            SettingsControl::popup(
                ctrl_x,
                y + (mod_metrics.row_h - mod_metrics.control_h) / 2.0,
                ctrl_w,
                mod_metrics.control_h,
                &mod_label_refs,
                0,
            ),
        )
        .1;
        bind_control(target, ui.modifier);
        SettingsSection::attach(
            switcher_view,
            NSRect::new(
                NSPoint::new(6.0, keyboard_card_bottom),
                NSSize::new(content_w - 12.0, keyboard_card_top - keyboard_card_bottom),
            ),
            &t("settings.header_keyboard"),
        );

        // ===== 鼠标页内容 mouse page content =====
        let mut y = mouse_doc_h;
        let mouse_title_h = SettingsPageHeader::attach(
            mouse_view,
            &t("settings.sidebar_mouse"),
            6.0,
            mouse_doc_h,
            content_w - 12.0,
        );
        y -= mouse_title_h + 18.0;

        // --- 启用鼠标控制(总开关,置于最顶) / Enable mouse control (topmost) ---
        // 小标题:本页与全 App 的区块都带一个短名词小标题(设备/滚动/指针/按键映射、剪贴板…),
        // 只有这张总开关卡片以前漏了,左上角看起来空一块。用「鼠标」而不是「鼠标控制」:比页面
        // 大标题短一档,复刻剪贴板页(小标题「剪贴板」/大标题「剪贴板历史」)的做法,也不会和
        // 行标题「启用鼠标控制」重复。
        //
        // Header: every section in this app carries a short-noun heading (Device / Scrolling /
        // Pointer / Button Mappings, Clipboard, ...), and this master-switch card was the only one
        // without it, which read as a blank spot at its top-left. "Mouse" rather than "Mouse
        // control": one notch shorter than the page title, the same way the clipboard page pairs
        // its "Clipboard" heading with the "Clipboard History" title, and it never repeats the row's
        // "Enable mouse control".
        y = layout.next_section_cursor(y);
        let mouse_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        let enable_mouse_bottom = y;
        ui.enable_mouse = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_enable_mouse"),
            &t("settings.desc_enable_mouse"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        // switch toggle 时实时更新 OK 按钮标题(确认 vs 确认并重启)。
        // Update OK button title in real time when the switch toggles (OK vs OK && Restart).
        let _: () = msg_send![ui.enable_mouse, setTarget: target];
        let _: () = msg_send![ui.enable_mouse, setAction: sel!(handleEnableMouseToggle:)];
        let _ = SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(enable_mouse_bottom)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(mouse_header_y) - layout.card_bottom(enable_mouse_bottom),
                ),
            ),
            &t("settings.header_mouse"),
        );

        // --- 设备选择器(内嵌下拉框,切换即时刷新其余控件) / Device picker (inline popup) ---
        y = layout.next_section_cursor(y);
        let device_header_y = y;
        // 下拉框:items 在 load_settings_values 里动态重建(设备列表可变)。
        // 首次创建放一个占位项,真正的内容在 load_settings_values -> rebuild_device_popup 填入。
        // Popup: items are rebuilt dynamically in load_settings_values (device list is mutable).
        // A placeholder is inserted here; the real items are filled by rebuild_device_popup.
        let device_labels: Vec<String> = crate::mouse::device::connected_devices()
            .iter()
            .map(|d| format!("{} ({:#x}:{:#x})", d.name, d.vendor_id, d.product_id))
            .collect();
        let device_label_refs: Vec<&str> = if device_labels.is_empty() {
            vec![""]
        } else {
            device_labels.iter().map(|s| s.as_str()).collect()
        };
        let device_metrics =
            SettingsSelect::metrics(ctrl_w, &device_label_refs, row_h, described_row_h);
        y = layout.next_row_cursor(y, device_metrics.row_h);
        let dev_popup = SettingsControl::popup(
            ctrl_x,
            y + (device_metrics.row_h - device_metrics.control_h) / 2.0,
            ctrl_w,
            device_metrics.control_h,
            &device_label_refs,
            0,
        );
        style_flat_popup(dev_popup);
        // 绑定 target/action:选择变化时即时刷新其余控件为该设备的有效值。
        // Bind target/action: on selection change, immediately refresh the other controls with
        // the selected device's effective values.
        let _: () = msg_send![dev_popup, setTarget: target];
        let _: () = msg_send![dev_popup, setAction: sel!(handleDeviceChanged:)];
        ui.device_indicator = SettingsRow::tall_with_height(
            mouse_view,
            label_x,
            y,
            label_w,
            device_metrics.row_h,
            &t("settings.header_mouse_device"),
            dev_popup,
        )
        .1;

        // --- 滚动模式 / Scroll mode ---
        let scroll_metrics =
            SettingsSelect::metrics(ctrl_w, &SCROLL_MODE_LABELS, row_h, described_row_h);
        y = layout.next_row_cursor(y, scroll_metrics.row_h);
        let scroll_popup = SettingsControl::popup(
            ctrl_x,
            y + (scroll_metrics.row_h - scroll_metrics.control_h) / 2.0,
            ctrl_w,
            scroll_metrics.control_h,
            &SCROLL_MODE_LABELS,
            0,
        );
        style_flat_popup(scroll_popup);
        ui.scroll_mode = SettingsRow::tall_with_height(
            mouse_view,
            label_x,
            y,
            label_w,
            scroll_metrics.row_h,
            &t("settings.row_scroll_mode"),
            scroll_popup,
        )
        .1;
        bind_control(target, ui.scroll_mode);
        // The HTML device card contains both rows, with one internal hairline between them.
        SettingsRow::separator_above_row(mouse_view, y, scroll_metrics.row_h, content_w);

        // --- 行数(按行模式) / Line count (line mode) ---
        // Keep this conditional row in the same card as Device and Scroll mode.
        // 将这个条件行放进与 Device、Scroll mode 相同的卡片中。
        y = layout.next_row_cursor(y, scroll_metrics.row_h);
        let line_count_separator =
            SettingsRow::separator_above_row(mouse_view, y, described_row_h, content_w);
        let (line_label, line_ctrl) = SettingsRow::tall(
            mouse_view,
            label_x,
            y,
            label_w,
            &t("settings.row_line_count"),
            // 整数滑块 1..=10(与 config 校验一致;对齐 LinearMouse By Lines 的滑块交互)。
            // 右侧留出读数宽度放只读数值 label 显示当前值(见 SettingsRow::slider_width)。
            // Leaves the readout's width on the right for the read-only value label (see
            // SettingsRow::slider_width). Integer slider 1..=10 (matches config validation;
            // mirrors LinearMouse's By Lines slider interaction).
            SettingsControl::slider(
                ctrl_x,
                y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                1,
                10,
                3,
                // 双击恢复默认行数(3)。
                // Double-click restores the default line count (3).
                Some(3.0),
            ),
        );
        ui.line_count = line_ctrl;
        ui.line_count_label = line_label;
        // 滑块右侧的只读数值 label:显示当前行数,拖动滑块时实时刷新。
        // Read-only value label right of the slider: shows the current line count, refreshed
        // live as the slider moves.
        ui.line_count_value_label = SettingsRow::attach_slider_readout(mouse_view, line_ctrl, 3);
        bind_control(target, ui.line_count);
        let device_card_parts = SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(device_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_device"),
        );
        let device_card = device_card_parts.card;
        let device_shadow = device_card_parts.shadow;
        // 行数行是条件行(只在 Line 模式显示):卡片是共用的设备卡片,隐藏时它的底边随之上收。
        // The line-count row is conditional (Line mode only): the card is the shared device card,
        // whose bottom edge rises when the row goes away.
        ui.line_count_block = CollapsibleRows::new(
            device_card,
            device_shadow,
            vec![
                ui.line_count,
                ui.line_count_label,
                ui.line_count_value_label,
            ],
            vec![line_count_separator],
            layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H,
        );

        // --- 滚动 Scrolling ---
        y = layout.next_section_cursor(y);
        let scrolling_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // reverse_scroll 开关:标题+副标题描述滚动方向,开关保留右侧内边距。
        // reverse_scroll switch: title + subtitle describe the scroll inversion; the switch
        // keeps the reference page's trailing inset.
        ui.reverse_scroll = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_reverse_scroll"),
            &t("settings.desc_reverse_scroll"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        bind_control(target, ui.reverse_scroll);
        SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(scrolling_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_scrolling"),
        );

        // --- 指针 Pointer ---
        y = layout.next_section_cursor(y);
        let pointer_header_y = y;
        y = layout.next_row_cursor(y, described_row_h);
        // disable_pointer_accel 开关:禁用系统鼠标加速,光标 1:1 线性跟踪。
        // 副标题说明线性跟踪的用途;开关与所有开关行一样保留右侧内边距。
        // disable_pointer_accel switch: disable system pointer acceleration for 1:1 linear
        // cursor tracking. The subtitle explains linear tracking; the switch keeps the same
        // trailing inset as every other switch row.
        ui.disable_pointer_accel = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_disable_pointer_accel"),
            &t("settings.desc_disable_pointer_accel"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        bind_control(target, ui.disable_pointer_accel);

        // --- 跟踪速度(仅"禁用指针加速(线性跟踪)"打开时显示)---
        // 线性跟踪下 HIDPointerAcceleration 的语义就是跟踪速度;开关关闭时该属性是加速
        // 曲线的强度,含义不同,所以这一行只在开关打开时出现(见 mouse/pointer.rs 模块注释)。
        // 0..=10 的连续滑块(无刻度吸附)+ 右侧只读数值。
        //
        // Tracking speed (shown only while "Disable pointer acceleration (linear tracking)" is
        // on). Under linear tracking HIDPointerAcceleration *is* the tracking speed; with the
        // switch off that property is the acceleration curve's strength, a different meaning, so
        // this row only appears while the switch is on (see the module comment in
        // mouse/pointer.rs). A continuous 0..=40 slider (no tick snapping) plus a read-only value
        // on the right.
        // 分割线的 y 必须传"线下方那一行"的 y(SettingsRow::separator 把线画在该行顶边上方
        // 3pt),否则会跑到卡片最顶上——设备卡内部的分割线也是这个写法。
        // The separator takes the y of the row BELOW the line (SettingsRow::separator draws it
        // 3pt above that row's top edge); any other y puts it at the card's top, which is what
        // the device card's internal dividers rely on too.
        y = layout.next_row_cursor(y, described_row_h);
        let pointer_accel_separator =
            SettingsRow::separator_above_row(mouse_view, y, described_row_h, content_w);
        let (pointer_accel_label, pointer_accel_slider) = SettingsRow::tall_with_height(
            mouse_view,
            label_x,
            y,
            label_w,
            described_row_h,
            &t("settings.row_pointer_tracking_speed"),
            // 右侧留出读数宽度放只读数值 label(与行数行同一布局,见 SettingsRow::slider_width)。
            // Leaves the readout's width on the right for the read-only value label (same layout
            // as the line-count row, see SettingsRow::slider_width).
            SettingsControl::double_slider(
                ctrl_x,
                y + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                crate::config::MOUSE_ACCELERATION_MIN,
                crate::config::MOUSE_ACCELERATION_MAX,
                crate::mouse::pointer::FALLBACK_ACCELERATION,
                // 双击恢复默认跟踪速度(1.00 = macOS 给鼠标键的出厂默认,也是本功能上线前的
                // 手感)。
                // Double-click restores the default tracking speed (1.00 = macOS's factory default
                // for the mouse key, i.e. what the pointer felt like before this setting existed).
                Some(crate::mouse::pointer::FALLBACK_ACCELERATION),
            ),
        );
        ui.pointer_accel_label = pointer_accel_label;
        ui.pointer_accel_slider = pointer_accel_slider;
        // 滑块右侧的只读数值 label:显示释放时的取值(2 位小数)。
        // Read-only value label right of the slider: shows the value on release (2 decimals).
        ui.pointer_accel_value_label = SettingsRow::attach_slider_readout(
            mouse_view,
            pointer_accel_slider,
            pointer_accel_display(crate::mouse::pointer::FALLBACK_ACCELERATION),
        );
        bind_control(target, ui.pointer_accel_slider);

        let pointer_card_parts = SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(pointer_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_pointer"),
        );
        // 跟踪速度行是条件行:把卡片、阴影、它自己的三个 view 与上方分割线交给组件管。
        // The tracking-speed row is conditional: hand the card, its shadow, the row's three views,
        // and the divider above it to the component.
        ui.pointer_accel_block = CollapsibleRows::new(
            pointer_card_parts.card,
            pointer_card_parts.shadow,
            vec![
                ui.pointer_accel_label,
                ui.pointer_accel_slider,
                ui.pointer_accel_value_label,
            ],
            vec![pointer_accel_separator],
            layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H,
        );

        // --- 按键映射 Button Mappings ---
        // 绑定区:"Enable button mappings" 描述行 + 嵌套表格卡片(圆角子表格 + 添加按钮)。
        // Button mappings: an "Enable button mappings" described row + a nested table card
        // (rounded sub-table + the add-mapping button).
        y = layout.next_section_cursor(y);
        let mappings_header_y = y;
        // "Enable button mappings" 描述行(HTML 卡片顶部),替代原来放在区块标题右侧的开关。
        // "Enable button mappings" described row (HTML card top), replacing the old switch
        // that sat on the section-header row's right edge.
        y = layout.next_row_cursor(y, described_row_h);
        ui.mapping_enabled = SettingsRow::described(
            mouse_view,
            label_x,
            y,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_mapping_enable"),
            &t("settings.desc_mapping_enable"),
            SettingsControl::switch(ctrl_x + ctrl_w, y + 10.0, row_h, false),
        );
        let _: () = msg_send![ui.mapping_enabled, setTarget: target];
        let _: () = msg_send![ui.mapping_enabled, setAction: sel!(handleMappingEnabledChanged:)];
        SettingsSection::attach(
            mouse_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(mappings_header_y) - layout.card_bottom(y),
                ),
            ),
            &t("settings.header_mouse_mappings"),
        );

        // --- 嵌套表格卡片(nested table card) ---
        y -= 24.0;
        let card_top = y;
        let card_w = content_w - 12.0;
        let card_h = MAPPING_PANEL_TOP
            + (MAPPING_HEADER_H + MAPPING_ROW_H * 3.0)
            + MAPPING_ACTION_TOP
            + MAPPING_ACTION_H
            + MAPPING_CARD_PAD_BOT;
        let card_bottom = card_top - card_h;
        // 外层卡片:白色卡片(与其它设置卡片一致),只有嵌套表格和添加按钮是灰色/深色。
        // The outer card is a white settings card (same as every other card); only the nested
        // table and the add button carry the gray "dark" treatment from the HTML reference.
        let card_bg: *mut AnyObject = msg_send![class!(NSView), alloc];
        // Align the mapping card with the other settings cards; the nested table keeps its own
        // inset so only the outer border expands to the shared content width.
        // 按键映射外框与其他设置卡片共用左右边界,内部表格继续保留自己的内缩。
        let card_bg: *mut AnyObject = msg_send![card_bg, initWithFrame: NSRect::new(NSPoint::new(6.0, card_bottom), NSSize::new(content_w - 12.0, card_h))];
        let _: () = msg_send![card_bg, setFlipped: true];
        let _: () = msg_send![card_bg, setAutoresizingMask: 0u64];
        let _: () = msg_send![card_bg, setWantsLayer: true];
        let bg_layer: *mut AnyObject = msg_send![card_bg, layer];
        let _: () = msg_send![bg_layer, setCornerRadius: 14.0f64];
        let _: () = msg_send![bg_layer, setMasksToBounds: true];
        let palette = settings_palette();
        crate::ffi::layer_set_background(bg_layer, crate::ffi::hex_to_cg_color(palette.card_bg));
        crate::ffi::layer_set_border(bg_layer, crate::ffi::hex_to_cg_color(palette.card_border));
        let _: () = msg_send![bg_layer, setBorderWidth: 1.0f64];
        // 嵌套的 `.mapping-table`:圆角描边子面板,铺在行后面,让映射区有 HTML 的表格观感。
        // The nested `.mapping-table`: a rounded, bordered sub-panel behind the rows, giving
        // the bindings the HTML reference's table look.
        let panel: *mut AnyObject = msg_send![class!(NSView), alloc];
        let panel: *mut AnyObject = msg_send![panel, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X, MAPPING_PANEL_TOP), NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_HEADER_H + MAPPING_ROW_H * 3.0))];
        let _: () = msg_send![panel, setWantsLayer: true];
        let panel_layer: *mut AnyObject = msg_send![panel, layer];
        let _: () = msg_send![panel_layer, setCornerRadius: 10.0f64];
        let _: () = msg_send![panel_layer, setMasksToBounds: true];
        crate::ffi::layer_set_background(
            panel_layer,
            crate::ffi::hex_to_cg_color(palette.field_bg),
        );
        crate::ffi::layer_set_border(
            panel_layer,
            crate::ffi::hex_to_cg_color(palette.card_border),
        );
        let _: () = msg_send![panel_layer, setBorderWidth: 1.0f64];
        let _: () = msg_send![card_bg, addSubview: panel];
        ui.mapping_panel = panel;
        release_obj(panel);
        // 表头带(.mapping-table thead)。
        // The header band (.mapping-table thead).
        let header_color = settings_text_color(SettingsTextRole::Secondary);
        let header_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 12.0f64];
        for (hx, hw, htext) in [
            (
                MAPPING_PANEL_X + MAPPING_CELL_X,
                120.0,
                t("settings.mapping_column_button"),
            ),
            (
                MAPPING_PANEL_X + MAPPING_CELL_X + 80.0,
                130.0,
                t("settings.mapping_column_action"),
            ),
        ] {
            let hlabel: *mut AnyObject = msg_send![class!(NSTextField), alloc];
            let hlabel: *mut AnyObject = msg_send![hlabel, initWithFrame: NSRect::new(NSPoint::new(hx, MAPPING_PANEL_TOP + 7.0), NSSize::new(hw, 18.0))];
            let hns = make_nsstring(&htext);
            let _: () = msg_send![hlabel, setStringValue: hns];
            CFRelease(hns as *const c_void);
            let _: () = msg_send![hlabel, setBezeled: false];
            let _: () = msg_send![hlabel, setDrawsBackground: false];
            let _: () = msg_send![hlabel, setEditable: false];
            let _: () = msg_send![hlabel, setFont: header_font];
            let _: () = msg_send![hlabel, setTextColor: header_color];
            let _: () = msg_send![card_bg, addSubview: hlabel];
            release_obj(hlabel);
        }
        // 表头下方 hairline。
        // Hairline under the header band.
        let header_line: *mut AnyObject = msg_send![class!(NSView), alloc];
        let header_line: *mut AnyObject = msg_send![header_line, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X + MAPPING_CELL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H - 1.0), NSSize::new(card_w - 2.0 * (MAPPING_PANEL_X + MAPPING_CELL_X), 1.0))];
        let _: () = msg_send![header_line, setWantsLayer: true];
        let header_line_layer: *mut AnyObject = msg_send![header_line, layer];
        let header_line_color: *mut AnyObject = msg_send![class!(NSColor), separatorColor];
        layer_set_background(header_line_layer, ns_color_to_cg(header_line_color));
        let _: () = msg_send![card_bg, addSubview: header_line];
        release_obj(header_line);
        // 空状态提示(无行时显示在子表格内)。
        // Empty-state hint (inside the sub-table when there are no rows).
        let empty: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let empty: *mut AnyObject = msg_send![empty, initWithFrame: NSRect::new(NSPoint::new(MAPPING_PANEL_X + MAPPING_CELL_X, MAPPING_PANEL_TOP + MAPPING_HEADER_H + (MAPPING_ROW_H * 3.0) / 2.0 - 9.0), NSSize::new(card_w - 2.0 * (MAPPING_PANEL_X + MAPPING_CELL_X), 18.0))];
        set_field(empty, 0);
        let _: () = msg_send![empty, setBezeled: false];
        let _: () = msg_send![empty, setDrawsBackground: false];
        let _: () = msg_send![empty, setEditable: false];
        let _: () = msg_send![empty, setAlignment: 1isize]; // center
        let empty_ns = make_nsstring(&t("settings.mapping_empty"));
        let _: () = msg_send![empty, setStringValue: empty_ns];
        CFRelease(empty_ns as *const c_void);
        let empty_color = settings_text_color(SettingsTextRole::Muted);
        let _: () = msg_send![empty, setTextColor: empty_color];
        let _: () = msg_send![empty, setHidden: true];
        let _: () = msg_send![card_bg, addSubview: empty];
        release_obj(empty);
        ui.mapping_empty = empty;
        // 添加按钮:卡片底部 action-row(全宽)。
        // Add-mapping button: full-width action row at the card bottom.
        let add_btn = SettingsButton::action(
            NSRect::new(
                NSPoint::new(
                    MAPPING_PANEL_X,
                    MAPPING_PANEL_TOP + MAPPING_HEADER_H + MAPPING_ROW_H * 3.0 + MAPPING_ACTION_TOP,
                ),
                NSSize::new(card_w - 2.0 * MAPPING_PANEL_X, MAPPING_ACTION_H),
            ),
            &t("settings.row_add_mapping"),
            target,
            sel!(handleAddMapping:),
            SettingsButtonRole::Compact,
        );
        let _: () = msg_send![card_bg, addSubview: add_btn];
        release_obj(add_btn);
        ui.add_mapping_button = add_btn;
        // 外层卡片 add 到页面。
        let _: () = msg_send![mouse_view, addSubview: card_bg];
        release_obj(card_bg);
        ui.mapping_card = card_bg;
        ui.mapping_scroll = std::ptr::null_mut();
        ui.mapping_doc = card_bg;
        let mouse_content_bottom = card_bottom;
        // 初始渲染当前设备的映射。
        // Render the current device's mappings initially.
        render_mapping_rows();

        // ===== 剪贴板历史页内容 clipboard page content =====
        // 独立布局游标(该页内容与鼠标页互不相关)。
        // Independent layout cursor (this page's content is unrelated to the mouse page).
        let mut cy = clipboard_doc_h;
        let clipboard_title_h = SettingsPageHeader::attach(
            clipboard_view,
            &t("settings.sidebar_clipboard"),
            6.0,
            clipboard_doc_h,
            content_w - 12.0,
        );
        cy -= clipboard_title_h + 18.0;
        let clipboard_header_y = cy - 18.0;
        // header 与首行间距与其他页一致(8 + row_h = 30):此前 16pt 挨得太近。
        // Header-to-first-row gap matches the other pages (8 + row_h = 30); it used to be
        // 16pt, too cramped.
        cy = layout.next_row_cursor_with_extra(cy, described_row_h, 18.0);
        // 启用开关 / master switch.
        // 启用开关 / master switch.
        // 英文 "Enable clipboard history"(实测 146pt)+ cell 内边距在 label_w=150 边缘,
        // 与 persist/move_used_to_top 行一起加宽到 225(见下方注释)。
        // English "Enable clipboard history" (measured 146pt) plus cell padding sits on
        // the label_w=150 edge; widen to 225 along with the persist/move_used_to_top rows.
        let clipboard_master_row_y = cy;
        ui.clipboard_enabled = SettingsRow::described(
            clipboard_view,
            label_x,
            cy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_clipboard_enabled"),
            &t("settings.desc_clipboard_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        let _: () = msg_send![ui.clipboard_enabled, setTarget: target];
        let _: () = msg_send![
            ui.clipboard_enabled,
            setAction: sel!(handleClipboardEnabledToggle:)
        ];
        SettingsSection::attach(
            clipboard_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(clipboard_master_row_y)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(clipboard_header_y)
                        - layout.card_bottom(clipboard_master_row_y),
                ),
            ),
            &t("settings.header_clipboard"),
        );
        // Keep the history controls in a second titled card, matching the switcher layout.
        // 其余历史记录设置单独成卡,并与切换器页面使用相同的小标题间距。
        cy = layout.next_section_cursor(cy);
        let clipboard_options_header_y = cy;
        // 置顶后选中项位置下拉框:项 = [跟随置顶, 保持当前位置];默认 index 0(跟随置顶),
        // 实际值由 load_settings_from 填充。
        // Pin-selection popup: items = [Follow the Pinned Entry, Keep Current Position];
        // default index 0 (follow); the real value is set by load_settings_from.
        let pin_labels = [
            t("settings.pin_follow_entry"),
            t("settings.pin_keep_position"),
        ];
        let pin_label_refs: Vec<&str> = pin_labels.iter().map(|s| s.as_str()).collect();
        let pin_metrics = SettingsSelect::metrics(ctrl_w, &pin_label_refs, row_h, described_row_h);
        cy = layout.next_row_cursor(cy, pin_metrics.row_h);
        ui.clipboard_pin_follow = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            220.0,
            pin_metrics.row_h,
            &t("settings.row_clipboard_pin_follow"),
            SettingsControl::popup(
                ctrl_x,
                cy + (pin_metrics.row_h - pin_metrics.control_h) / 2.0,
                ctrl_w,
                pin_metrics.control_h,
                &pin_label_refs,
                0,
            ),
        );
        bind_control(target, ui.clipboard_pin_follow);
        cy = layout.next_row_cursor(cy, pin_metrics.row_h);
        SettingsRow::separator_above_row(clipboard_view, cy, pin_metrics.row_h, content_w);
        // 保存历史开关(持久化到磁盘,重启不丢;明文落盘,隐私风险见 README)。
        // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
        // privacy implications are documented in the README).
        // 保存历史开关(持久化到磁盘,重启不丢;明文落盘,隐私风险见 README)。
        // 中文标签"保存剪贴板历史记录到磁盘"(11 字)与英文 "Save clipboard history
        // to disk" 都超出默认 label_w=150(渲染截断),该行加宽到 225——与
        // show_minimized 行同款处理;开关保留右侧内边距,避免与边缘重叠。
        // Persist switch (saved to disk, survives restarts; plaintext on disk -- the
        // privacy implications are documented in the README). The Chinese (11 CJK
        // chars) and English labels both exceed the default label_w=150 (rendered
        // truncated), so this row widens its label to 225 -- same as the
        // show_minimized row; the switch keeps the trailing inset and stays clear of the edge.
        ui.clipboard_persist = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            220.0,
            described_row_h,
            &t("settings.row_clipboard_persist"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        bind_control(target, ui.clipboard_persist);
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
        // 显示来源应用 / show the source app.
        ui.clipboard_show_source_app = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            label_w,
            described_row_h,
            &t("settings.row_clipboard_show_source_app"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        bind_control(target, ui.clipboard_show_source_app);
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
        // 使用后移到最前(粘贴是否重排历史;默认开 = 保持现状)。
        // Move used entries to the top (whether pasting reorders the history; on by
        // default = current behavior).
        // 英文 "Move used entries to top"(实测 150.3pt)超出 label_w=150 渲染截断
        // (用户切英文后看到 "move used entries to"),加宽到 225。
        // English "Move used entries to top" (measured 150.3pt) exceeds label_w=150 and
        // rendered truncated ("move used entries to" after switching to English), widened
        // to 225.
        ui.clipboard_move_used_to_top = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            220.0,
            described_row_h,
            &t("settings.row_clipboard_move_used_to_top"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        bind_control(target, ui.clipboard_move_used_to_top);
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
        // 粘贴后删除(Option+回车/点击 = 一次性粘贴)。默认关——销毁性手势,显式选择
        // 加入。说明副标题已不再渲染(见 add_described_row 的 _subtitle),手势提示
        // 直接并入标签;文本宽度沿用总开关 described 行的全宽,避免长标签截断。
        // Delete after paste (Option+Enter/click = one-shot paste). Off by default -- a
        // destructive gesture, strictly opt-in. Row subtitles are no longer rendered (see
        // add_described_row's _subtitle), so the gesture hint lives in the label itself;
        // the text width follows the master described row's full width so the long label
        // never truncates.
        ui.clipboard_delete_after_paste = SettingsRow::described(
            clipboard_view,
            label_x,
            cy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_clipboard_delete_after_paste"),
            "",
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        bind_control(target, ui.clipboard_delete_after_paste);
        cy = layout.next_row_cursor(cy, described_row_h);
        let clear_pasteboard_separator =
            SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
        // 这一行是上面开关的子项(标签内缩):它只在"粘贴后删除条目"打开时出现,所以走条件行
        // 组件(整行显隐 + 下方分组补位),而不是置灰。
        // This row is a child of the switch above (indented label): it only appears while "delete
        // entry after paste" is on, so it goes through the conditional-row component (whole row
        // shown/hidden, sections below closing the gap) rather than being greyed out.
        let (clear_pasteboard_label, clear_pasteboard_switch) = SettingsRow::tall_with_height(
            clipboard_view,
            label_x + 18.0,
            cy,
            ctrl_x - label_x - 36.0,
            described_row_h,
            &t("settings.row_clipboard_clear_system_pasteboard_after_paste"),
            SettingsControl::switch(ctrl_x + ctrl_w, cy, row_h, false),
        );
        ui.clipboard_clear_system_pasteboard_after_paste = clear_pasteboard_switch;
        bind_control(target, ui.clipboard_clear_system_pasteboard_after_paste);
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
        // 最大条数(数字输入)/ max entries (number input).
        ui.clipboard_max_entries = SettingsRow::plain(
            clipboard_view,
            label_x,
            cy,
            label_w,
            described_row_h,
            &t("settings.row_clipboard_max_entries"),
            SettingsControl::text_input(ctrl_x, cy, ctrl_w, row_h, "50"),
        );
        cy = layout.next_row_cursor(cy, described_row_h);
        SettingsRow::separator_above_row(clipboard_view, cy, described_row_h, content_w);
        // 自动过期天数滑块:0..=7,0 = 永不过期;右侧显示当前值。
        // Auto-expire days slider: 0..=7, where 0 means never; the current value is shown on
        // the right.
        let (_, auto_expire_slider) = SettingsRow::tall(
            clipboard_view,
            label_x,
            cy,
            label_w,
            &t("settings.row_clipboard_auto_expire_days"),
            SettingsControl::slider(
                ctrl_x,
                cy + 10.0,
                SettingsRow::slider_width(ctrl_w),
                row_h,
                CLIPBOARD_AUTO_EXPIRE_MIN,
                CLIPBOARD_AUTO_EXPIRE_MAX,
                CLIPBOARD_AUTO_EXPIRE_DEFAULT,
                // 双击恢复默认天数(3 天)。
                // Double-click restores the default (3 days).
                Some(CLIPBOARD_AUTO_EXPIRE_DEFAULT as f64),
            ),
        );
        ui.clipboard_auto_expire_days = auto_expire_slider;
        ui.clipboard_auto_expire_days_value_label = SettingsRow::attach_slider_readout(
            clipboard_view,
            auto_expire_slider,
            CLIPBOARD_AUTO_EXPIRE_DEFAULT,
        );
        bind_control(target, ui.clipboard_auto_expire_days);
        let clipboard_options_card_bottom = layout.card_bottom(cy);
        let clipboard_options_card_parts = SettingsSection::attach(
            clipboard_view,
            NSRect::new(
                NSPoint::new(6.0, clipboard_options_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(clipboard_options_header_y) - clipboard_options_card_bottom,
                ),
            ),
            &t("settings.header_clipboard_options"),
        );
        // "同时删除系统剪贴板中对应条目"整行随"粘贴后删除条目"显隐(单行高)。
        // The "clear the matching system-pasteboard entry" row follows "delete entry after paste"
        // (one row tall).
        ui.clipboard_delete_block = CollapsibleRows::new(
            clipboard_options_card_parts.card,
            clipboard_options_card_parts.shadow,
            vec![clear_pasteboard_label, clear_pasteboard_switch],
            vec![clear_pasteboard_separator],
            layout.row_gap + SettingsLayout::SINGLE_LINE_ROW_H,
        );

        // ===== 窗口控制页内容 window control page content =====
        // 独立布局游标(该页内容与剪贴板页互不相关)。
        // Independent layout cursor (unrelated to the clipboard page).
        let mut wy = window_control_doc_h;
        let window_control_title_h = SettingsPageHeader::attach(
            window_control_view,
            &t("settings.sidebar_window_control"),
            6.0,
            window_control_doc_h,
            content_w - 12.0,
        );
        wy -= window_control_title_h + 18.0;
        let window_control_header_y = wy - 18.0;
        // header 与首行间距与剪贴板页一致(18 + row_gap)。
        // Header-to-first-row gap matches the clipboard page (18 + row_gap).
        wy = layout.next_row_cursor_with_extra(wy, described_row_h, 18.0);
        // 启用窗口控制(总开关):Option+方向键的全局拦截默认关闭,由用户显式开启。
        // Enable window control (master switch): the global Option+arrow interception is off
        // by default and must be explicitly opted in.
        ui.window_control_enabled = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_enabled"),
            &t("settings.desc_window_control_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        let _: () = msg_send![ui.window_control_enabled, setTarget: target];
        let _: () = msg_send![
            ui.window_control_enabled,
            setAction: sel!(handleWindowControlEnabledToggle:)
        ];
        SettingsSection::attach(
            window_control_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(wy)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(window_control_header_y) - layout.card_bottom(wy),
                ),
            ),
            &t("settings.header_window_control"),
        );

        // 方向快捷键单独成一块卡片,总开关与具体方向配置互不混排。
        // Put the direction shortcuts in their own card so the master switch is separate from
        // the per-direction settings.
        wy = layout.next_section_cursor(wy);
        let window_control_shortcuts_header_y = wy;
        wy = layout.next_row_cursor(wy, described_row_h);
        ui.window_control_up = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_up"),
            &t("settings.desc_window_control_up"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_up);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_down = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_down"),
            &t("settings.desc_window_control_down"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_down);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_left = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_left"),
            &t("settings.desc_window_control_left"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_left);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_right = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_right"),
            &t("settings.desc_window_control_right"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_right);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_display_up = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_display_up"),
            &t("settings.desc_window_control_display_up"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_display_up);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_display_down = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_display_down"),
            &t("settings.desc_window_control_display_down"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_display_down);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_display_left = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_display_left"),
            &t("settings.desc_window_control_display_left"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_display_left);
        wy = layout.next_row_cursor(wy, described_row_h);
        SettingsRow::separator_above_row(window_control_view, wy, described_row_h, content_w);
        ui.window_control_display_right = SettingsRow::described(
            window_control_view,
            label_x,
            wy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_window_control_display_right"),
            &t("settings.desc_window_control_display_right"),
            SettingsControl::switch(ctrl_x + ctrl_w, wy, row_h, false),
        );
        bind_control(target, ui.window_control_display_right);
        let window_control_shortcuts_card_bottom = layout.card_bottom(wy);
        SettingsSection::attach(
            window_control_view,
            NSRect::new(
                NSPoint::new(6.0, window_control_shortcuts_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(window_control_shortcuts_header_y)
                        - window_control_shortcuts_card_bottom,
                ),
            ),
            &t("settings.header_window_control_shortcuts"),
        );

        // ===== 快捷操作页内容 quick actions page content =====
        // 独立布局游标(该页内容与窗口控制页互不相关)。
        // Independent layout cursor (unrelated to the window-control page).
        let mut qy = quick_actions_doc_h;
        let quick_actions_title_h = SettingsPageHeader::attach(
            quick_actions_view,
            &t("settings.sidebar_quick_actions"),
            6.0,
            quick_actions_doc_h,
            content_w - 12.0,
        );
        qy -= quick_actions_title_h + 18.0;
        let quick_actions_header_y = qy - 18.0;
        // header 与首行间距与窗口控制页一致(18 + row_gap)。
        // Header-to-first-row gap matches the window-control page (18 + row_gap).
        qy = layout.next_row_cursor_with_extra(qy, described_row_h, 18.0);
        // 启用快捷操作(总开关):Option+I/E/D/L 全局拦截默认关闭,由用户显式开启。
        // Enable quick actions (master switch): the global Option+I/E/D/L interception is off
        // by default and must be explicitly opted in.
        ui.quick_actions_enabled = SettingsRow::described(
            quick_actions_view,
            label_x,
            qy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_quick_actions_enabled"),
            &t("settings.desc_quick_actions_enabled"),
            SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
        );
        let _: () = msg_send![ui.quick_actions_enabled, setTarget: target];
        let _: () = msg_send![
            ui.quick_actions_enabled,
            setAction: sel!(handleQuickActionsEnabledToggle:)
        ];
        SettingsSection::attach(
            quick_actions_view,
            NSRect::new(
                NSPoint::new(6.0, layout.card_bottom(qy)),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(quick_actions_header_y) - layout.card_bottom(qy),
                ),
            ),
            &t("settings.header_quick_actions"),
        );

        // 四个动作开关单独成块卡片,总开关与具体动作互不混排(与窗口控制页一致)。
        // Put the four action switches in their own card so the master switch stays separate
        // from the per-action settings (matching the window-control page).
        qy = layout.next_section_cursor(qy);
        let quick_actions_shortcuts_header_y = qy;
        qy = layout.next_row_cursor(qy, described_row_h);
        ui.quick_actions_open_settings = SettingsRow::described(
            quick_actions_view,
            label_x,
            qy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_quick_action_open_settings"),
            &t("settings.desc_quick_action_open_settings"),
            SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
        );
        bind_control(target, ui.quick_actions_open_settings);
        qy = layout.next_row_cursor(qy, described_row_h);
        SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
        ui.quick_actions_open_finder = SettingsRow::described(
            quick_actions_view,
            label_x,
            qy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_quick_action_open_finder"),
            &t("settings.desc_quick_action_open_finder"),
            SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
        );
        bind_control(target, ui.quick_actions_open_finder);
        qy = layout.next_row_cursor(qy, described_row_h);
        SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
        ui.quick_actions_show_desktop = SettingsRow::described(
            quick_actions_view,
            label_x,
            qy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_quick_action_show_desktop"),
            &t("settings.desc_quick_action_show_desktop"),
            SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
        );
        bind_control(target, ui.quick_actions_show_desktop);
        qy = layout.next_row_cursor(qy, described_row_h);
        SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
        ui.quick_actions_lock_screen = SettingsRow::described(
            quick_actions_view,
            label_x,
            qy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_quick_action_lock_screen"),
            &t("settings.desc_quick_action_lock_screen"),
            SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
        );
        bind_control(target, ui.quick_actions_lock_screen);
        qy = layout.next_row_cursor(qy, described_row_h);
        SettingsRow::separator_above_row(quick_actions_view, qy, described_row_h, content_w);
        ui.quick_actions_locate_pointer = SettingsRow::described(
            quick_actions_view,
            label_x,
            qy,
            ctrl_x - label_x - 18.0,
            described_row_h,
            &t("settings.row_quick_action_locate_pointer"),
            &t("settings.desc_quick_action_locate_pointer"),
            SettingsControl::switch(ctrl_x + ctrl_w, qy, row_h, false),
        );
        bind_control(target, ui.quick_actions_locate_pointer);
        let quick_actions_card_bottom = layout.card_bottom(qy);
        SettingsSection::attach(
            quick_actions_view,
            NSRect::new(
                NSPoint::new(6.0, quick_actions_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(quick_actions_shortcuts_header_y) - quick_actions_card_bottom,
                ),
            ),
            &t("settings.header_quick_actions_shortcuts"),
        );

        // ===== About page: page-header + App and Updates cards from preview (10). =====
        let header_top = about_doc_h - 68.0;
        add_about_app_icon(about_view, label_x, header_top - 58.0);

        let about_title: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let about_title: *mut AnyObject = msg_send![
            about_title,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 73.0, header_top - 33.0),
                NSSize::new(content_w - 73.0 - label_x, 28.0),
            )
        ];
        set_field(about_title, "Oh My Tab");
        let _: () = msg_send![about_title, setBezeled: false];
        let _: () = msg_send![about_title, setDrawsBackground: false];
        let _: () = msg_send![about_title, setEditable: false];
        let about_title_font: *mut AnyObject =
            msg_send![class!(NSFont), boldSystemFontOfSize: 24.0f64];
        let _: () = msg_send![about_title, setFont: about_title_font];
        let _: () = msg_send![about_view, addSubview: about_title];
        release_obj(about_title);

        let about_subtitle: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let about_subtitle: *mut AnyObject = msg_send![
            about_subtitle,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x + 73.0, header_top - 53.0),
                NSSize::new(content_w - 73.0 - label_x, 18.0),
            )
        ];
        set_field(
            about_subtitle,
            tf(
                "settings.version_label",
                &[("version", env!("CARGO_PKG_VERSION"))],
            ),
        );
        let _: () = msg_send![about_subtitle, setBezeled: false];
        let _: () = msg_send![about_subtitle, setDrawsBackground: false];
        let _: () = msg_send![about_subtitle, setEditable: false];
        let about_subtitle_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 13.0f64];
        let _: () = msg_send![about_subtitle, setFont: about_subtitle_font];
        let about_subtitle_color = settings_text_color(SettingsTextRole::Muted);
        let _: () = msg_send![about_subtitle, setTextColor: about_subtitle_color];
        let _: () = msg_send![about_view, addSubview: about_subtitle];
        release_obj(about_subtitle);
        ui.about_subtitle = about_subtitle;

        // Transparent hit area for the five-click build-version easter egg. It is added after
        // the labels so it receives clicks across the whole header without changing its visuals.
        // 透明点击区域用于五击显示 build-version 的彩蛋。放在文字之后，覆盖整个头部但不改变外观。
        let about_header_hit: *mut AnyObject = msg_send![about_header_click_view_class(), alloc];
        let about_header_hit: *mut AnyObject = msg_send![
            about_header_hit,
            initWithFrame: NSRect::new(
                NSPoint::new(6.0, header_top - 64.0),
                NSSize::new(content_w - 12.0, 66.0),
            )
        ];
        let _: () = msg_send![about_view, addSubview: about_header_hit];
        release_obj(about_header_hit);

        let mut ay = header_top - 88.0;
        // Keep the App section title close to its card, matching the spacing used by the
        // other settings pages. The About card has three rows, so its content cursor is lower
        // than a normal section header; placing the title at the old cursor left a large void.
        // 让 App 分组标题贴近下方卡片,与其他设置页保持一致。About 卡片有三行内容,其内容
        // 游标比普通区块标题更低;沿用旧游标会在标题和卡片之间留下过大的空白。
        let app_label_y = ay - 35.0;
        ay -= 27.0;
        let about_row_step = layout.row_gap + described_row_h;
        let website_y = ay - about_row_step;
        // Keep every About row on the same two-column grid: label on the left, value on the right.
        // About 页面所有行统一使用两列网格：左侧标签，右侧值。
        let about_value_x = label_x + 145.0;
        let about_value_w = (content_w - 2.0 * label_x - 145.0).max(1.0);
        SettingsRow::plain(
            about_view,
            label_x,
            website_y,
            label_w,
            described_row_h,
            &t("settings.website_label"),
            SettingsControl::external_link(
                about_value_x,
                website_y,
                about_value_w,
                row_h,
                &t("settings.website_url"),
                0,
            ),
        );
        let github_y = website_y - about_row_step;
        SettingsRow::separator_above_row(about_view, github_y, described_row_h, content_w);
        SettingsRow::plain(
            about_view,
            label_x,
            github_y,
            label_w,
            described_row_h,
            &t("settings.github_label"),
            SettingsControl::external_link(
                about_value_x,
                github_y,
                about_value_w,
                row_h,
                &t("settings.github_url"),
                1,
            ),
        );
        let version_y = github_y - about_row_step;
        SettingsRow::separator_above_row(about_view, version_y, described_row_h, content_w);
        SettingsRow::plain(
            about_view,
            label_x,
            version_y,
            label_w,
            described_row_h,
            &t("settings.version_label_short"),
            SettingsControl::value_label(
                about_value_x,
                version_y,
                120.0,
                row_h,
                env!("CARGO_PKG_VERSION"),
            ),
        );
        let app_card_bottom = layout.card_bottom(version_y);
        SettingsSection::attach(
            about_view,
            NSRect::new(
                NSPoint::new(6.0, app_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(app_label_y) - app_card_bottom,
                ),
            ),
            &t("settings.section_app"),
        );

        ay = version_y - 42.0;
        let updates_label_y = ay - 11.0;
        ay -= 27.0;
        let update_row_y = ay - 44.0;
        ui.update_auto_check = SettingsRow::described(
            about_view,
            label_x,
            update_row_y,
            (ctrl_x + ctrl_w) - label_x - 70.0,
            described_row_h,
            &t("settings.row_update_auto_check"),
            &t("settings.desc_update_auto_check"),
            SettingsControl::switch(ctrl_x + ctrl_w, update_row_y + 10.0, row_h, false),
        );
        bind_control(target, ui.update_auto_check);
        // 自动下载并安装更新开关,位于「自动检查更新」与「检查更新」之间。
        // Automatically-download-and-install switch, between auto-check and the check button.
        let download_row_y = update_row_y - described_row_h;
        ui.update_auto_download = SettingsRow::described(
            about_view,
            label_x,
            download_row_y,
            (ctrl_x + ctrl_w) - label_x - 70.0,
            described_row_h,
            &t("settings.row_update_auto_download"),
            &t("settings.desc_update_auto_download"),
            SettingsControl::switch(ctrl_x + ctrl_w, download_row_y + 10.0, row_h, false),
        );
        bind_control(target, ui.update_auto_download);
        // Keep the two update toggles visually grouped with the same inset divider used by other
        // multi-row cards. The rows are contiguous here, so the divider sits at their shared edge.
        // 两个更新开关属于同一张多行卡片，复用其他卡片的内缩分割线；两行相邻，分割线放在共享边界。
        SettingsRow::separator(about_view, update_row_y, content_w);
        // 检查更新:加高的全宽按钮,标题随流程在「检查更新…/检查中…/已是最新版本」间切换。
        // Check for updates: a taller full-width button whose title switches between
        // "Check for Updates…", "Checking…", and "You're up to date".
        let check_button_h = 38.0;
        // Keep the check button directly below the second toggle. When the inline update host
        // replaces it, the result content can then start directly at the divider without retaining
        // the old button's vertical slot or its extra 14pt spacer.
        // 检查更新按钮紧贴第二个开关行下方。内联更新宿主替换按钮后，结果内容直接从分割线开始，
        // 不再保留旧按钮的高度占位和额外 14pt 间距。
        let check_button_y = download_row_y - check_button_h;
        let check_button = SettingsButton::action(
            NSRect::new(
                NSPoint::new(label_x, check_button_y),
                NSSize::new(content_w - 2.0 * label_x, check_button_h),
            ),
            &t("settings.btn_check_for_updates"),
            target,
            sel!(handleCheckForUpdates:),
            SettingsButtonRole::Action,
        );
        let _: () = msg_send![check_button, setTag: -3isize];
        let check_layer: *mut AnyObject = msg_send![check_button, layer];
        if !check_layer.is_null() {
            layer_set_background(
                check_layer,
                crate::ffi::hex_to_cg_color(settings_palette().button_bg),
            );
        }
        let _: () = msg_send![about_view, addSubview: check_button];
        ui.update_check_button = check_button;
        release_obj(check_button);
        // 内联更新流程的宿主容器:更新状态/进度/按钮渲染进这个 NSView,不再弹独立窗口。
        // Inline update-flow host container: update status/progress/buttons render here instead of
        // a separate NSWindow. Empty and hidden by default, so the About page stays compact; an
        // active flow expands the card + host via expand_update_section.
        // 宿主直接占用「检查更新」按钮的位置,内容用顶向下坐标排布,更新状态会替换按钮而不是
        // 追加在按钮下方。初始高度为 0,故 origin.y 即顶边。
        // The host occupies the check button's position; its top-down content replaces the button
        // instead of being appended below it. With an initial height of 0, origin.y is the top.
        let compact_host_h = 0.0;
        let host_origin_y = check_button_y;
        let update_host: *mut AnyObject = msg_send![widgets::flipped_settings_view_class(), alloc];
        let update_host: *mut AnyObject = msg_send![
            update_host,
            initWithFrame: NSRect::new(
                NSPoint::new(label_x, host_origin_y),
                NSSize::new(content_w - 2.0 * label_x, compact_host_h),
            )
        ];
        let _: () = msg_send![update_host, setHidden: true];
        let _: () = msg_send![about_view, addSubview: update_host];
        release_obj(update_host);
        ui.update_host = update_host;
        ui.update_host_origin_y = host_origin_y;
        ui.update_host_window = window;
        crate::updater::set_update_host(update_host, window, check_button);
        // 收起时的卡片下沿紧贴「检查更新」按钮下方 10pt,默认不为内联区域预留大块空白。
        // The collapsed card bottom hugs the check button with a 10pt inset; the inline area is
        // not reserved by default, avoiding a large blank.
        let compact_card_bottom = check_button_y - 10.0;
        let update_card_parts = SettingsSection::attach(
            about_view,
            NSRect::new(
                NSPoint::new(6.0, compact_card_bottom),
                NSSize::new(
                    content_w - 12.0,
                    layout.card_top(updates_label_y) - compact_card_bottom,
                ),
            ),
            &t("settings.section_updates"),
        );
        let update_card = update_card_parts.card;
        let update_card_shadow = update_card_parts.shadow;
        ui.update_card = update_card;
        ui.update_card_shadow = update_card_shadow;
        // Reuse the same full-width card divider as the boundary between grouped settings rows.
        // It is hidden while compact and revealed only when the inline update result replaces the
        // check button area, so the collapsed About page does not gain an empty separator.
        // 复用分组设置行之间的整宽卡片分割线。收起时隐藏，内联更新结果替换检查按钮区域后才显示，
        // 避免紧凑的 About 页面凭空多出一条空分割线。
        let update_divider = SettingsRow::separator(about_view, download_row_y, content_w);
        let _: () = msg_send![update_divider, setHidden: true];
        ui.update_divider = update_divider;
        ui.update_card_compact_h = {
            let compact_frame: NSRect = msg_send![update_card, frame];
            compact_frame.size.height
        };

        // banner 最后添加:作为 general_view 的最后一个 subview,保证在内容之上(缺权限时覆盖顶部)。
        // Added last: as general_view's final subview so it floats above the content (when
        // permission is missing). It occupies no layout space, so no top gap when hidden.
        let _: () = msg_send![general_view, addSubview: banner];
        release_obj(banner);

        // Let AppKit finish its first layout pass before validating the actual view tree. Do not
        // shrink the provisional documents here: their children are top-anchored with
        // autoresizing masks, and post-hoc height fitting can move them a second time.
        // 先让 AppKit 完成首次布局，再校验真实 view tree。这里不收缩临时 document 高度：子视图
        // 使用顶部锚定 autoresizing，布局后再改高度会触发第二次位移。
        let _: () = msg_send![window, layoutIfNeeded];
        for (name, page) in [
            (
                "general",
                SettingsPage {
                    scroll: general_root,
                    document: general_view,
                },
            ),
            (
                "switcher",
                SettingsPage {
                    scroll: switcher_root,
                    document: switcher_view,
                },
            ),
            (
                "mouse",
                SettingsPage {
                    scroll: mouse_root,
                    document: mouse_view,
                },
            ),
            (
                "clipboard",
                SettingsPage {
                    scroll: clipboard_root,
                    document: clipboard_view,
                },
            ),
            (
                "quick-actions",
                SettingsPage {
                    scroll: quick_actions_root,
                    document: quick_actions_view,
                },
            ),
            (
                "about",
                SettingsPage {
                    scroll: about_root,
                    document: about_view,
                },
            ),
        ] {
            page.validate(name);
        }
        let update_host_frame: NSRect = msg_send![ui.update_host, frame];
        ui.update_host_origin_y = update_host_frame.origin.y;

        // --- 恢复本页默认设置(每页内容末尾各一个,随页面滚动)---
        // Each page embeds its own "Restore Page Defaults" control at the end of its content,
        // so it scrolls with the page. Only the selected page's control is visible/clickable
        // (pages toggle visibility), so the confirm handler can act on the selected tab.
        // 每页文档只在自己被选中时可见可点,确认回调因此可以按当前选中页处理。
        let page_roots = [
            general_root,
            switcher_root,
            mouse_root,
            clipboard_root,
            window_control_root,
            quick_actions_root,
            about_root,
        ];
        let page_bottoms = [
            general_content_bottom,
            keyboard_card_bottom,
            mouse_content_bottom,
            clipboard_options_card_bottom,
            window_control_shortcuts_card_bottom,
            quick_actions_card_bottom,
            compact_card_bottom,
        ];
        for (index, root) in page_roots.iter().enumerate() {
            let document: *mut AnyObject = msg_send![*root, documentView];
            ui.page_restores[index] = RestoreDefaultsControl::build_for_page(
                document,
                target,
                6.0,
                page_bottoms[index] - 16.0,
                content_w - 12.0,
            );
        }

        // --- 数字文本框通知:输入中防抖应用,失焦/回车立即提交 ---
        // --- Numeric text-field notifications: debounced apply while typing, immediate commit
        // on blur / Enter ---
        {
            let center: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
            for (field_ptr, change_sel, end_sel) in [
                (
                    ui.corner_radius,
                    sel!(handleControlTextDidChange:),
                    sel!(handleControlTextDidEndEditing:),
                ),
                (
                    ui.clipboard_max_entries,
                    sel!(handleControlTextDidChange:),
                    sel!(handleControlTextDidEndEditing:),
                ),
            ] {
                let change_name = make_nsstring("NSControlTextDidChangeNotification");
                let _: () = msg_send![
                    center,
                    addObserver: target,
                    selector: change_sel,
                    name: change_name,
                    object: field_ptr
                ];
                CFRelease(change_name as *const c_void);
                let end_name = make_nsstring("NSControlTextDidEndEditingNotification");
                let _: () = msg_send![
                    center,
                    addObserver: target,
                    selector: end_sel,
                    name: end_name,
                    object: field_ptr
                ];
                CFRelease(end_name as *const c_void);
            }
        }

        with_settings_ui(|slot| *slot = Some(ui));
    }
}

/// Detach global references owned by a settings window before it is released or replaced.
/// Keep the traffic-light baseline for in-place redraws; remove it only when the window is
/// actually released.
/// 在释放或替换设置窗口前,解除由该窗口持有的全局引用。原位重绘时保留红绿灯基准,
/// 只有窗口真正释放时才移除。
unsafe fn detach_settings_window_runtime(ui: &SettingsUi, remove_traffic_light_origin: bool) {
    close_glass_tint_panel(ui.glass_tint);
    if remove_traffic_light_origin {
        TRAFFIC_LIGHT_BASE_ORIGINS
            .lock()
            .unwrap()
            .remove(&(ui.window as usize));
    }
    // 先让 update 模块解除对宿主视图的引用,再释放窗口,避免它写入已释放视图。
    // Detach the updater's host references before releasing the window so it never touches
    // a deallocated view.
    crate::updater::clear_update_host();
    SettingsRow::clear_runtime_registry();
    widgets::clear_settings_select_registry();
    tooltip::SettingsTooltip::clear_runtime_registries();
    SIDEBAR_TITLE_LABELS.lock().unwrap().clear();
    SIDEBAR_ICON_VIEWS.lock().unwrap().clear();
    SIDEBAR_UPDATE_DOTS.lock().unwrap().clear();
}

/// 作废缓存的设置窗口(释放并置 None),下次打开时按当前 locale 重建。
/// Invalidate the cached settings window (release + set None) so it is rebuilt with the
/// current locale on next open. 用于 locale 变更后让设置窗口标签换语言。
pub(crate) fn invalidate_settings_window() {
    SYSTEM_APPEARANCE_REBUILD_PENDING.store(false, Ordering::SeqCst);
    set_text_input_active(false);
    let ui = with_settings_ui(|slot| slot.take());
    if let Some(u) = ui {
        unsafe {
            detach_settings_window_runtime(&u, true);
            // 窗口 alloc 是 +1且 setReleasedWhenClosed:false,需手动 release 一次;
            // 其子控件已由父视图持有,随窗口 dealloc 释放。
            // The window is alloc +1 with setReleasedWhenClosed:false, so release once manually;
            // its subviews are retained by the parent view and dealloc with the window.
            let _: () = msg_send![u.window, orderOut: std::ptr::null::<AnyObject>()];
            release_obj(u.window);
        }
        // 窗口被作废(销毁),切回 .accessory(可能 locale 变更时设置正开着)。
        // The window is invalidated/destroyed; flip back to .accessory (it may have been open
        // during a locale change).
        crate::set_settings_activation_policy(false);
    }
}
