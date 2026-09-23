//! 设置窗口 · window:窗口类/生命周期(显示、隐藏、侧栏切换)与内容构建。
//! Settings window classes and lifecycle (show/hide/sidebar switching) plus content construction.

use super::*;

/// Show the permission banner in its own top strip and reserve that strip above General.
/// 将权限提示显示在独立的顶部区域，并从通用页滚动视口中扣除对应高度。
unsafe fn set_permission_banner_visible(ui: &SettingsUi, visible: bool) {
    if ui.permission_warning_view.is_null() || ui.general_view.is_null() {
        return;
    }

    let hidden: bool = msg_send![ui.permission_warning_view, isHidden];
    if hidden == !visible {
        return;
    }

    let banner_frame: NSRect = msg_send![ui.permission_warning_view, frame];
    let mut general_frame: NSRect = msg_send![ui.general_view, frame];
    let height_delta = if visible {
        -banner_frame.size.height
    } else {
        banner_frame.size.height
    };
    general_frame.size.height = (general_frame.size.height + height_delta).max(1.0);
    let _: () = msg_send![ui.general_view, setFrame: general_frame];
    let _: () = msg_send![ui.permission_warning_view, setHidden: !visible];
}

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
            let show_permission_banner = if idx == 0 {
                let is_permission_migration =
                    crate::update_notice::needs_permission_migration_copy();
                let has_required_permissions = has_accessibility_permission()
                    && (!is_permission_migration || crate::thumbnail::capture_allowed());
                !has_required_permissions
            } else {
                false
            };
            set_permission_banner_visible(ui, show_permission_banner);
            if idx == 6 {
                refresh_permission_statuses(ui);
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
    // A2 层 E2E:选中态与高亮 pill 已归位,写一条几何快照供 scripts/e2e 断言。
    // 必须放在 with_settings_ui 闭包**之外**:闭包内再借一次会被重入保护静默挡掉。
    // A2 E2E: the selection and the highlight pill are parked, so write a geometry snapshot for
    // scripts/e2e. It must sit outside the with_settings_ui closure -- a nested borrow there is
    // silently swallowed by the reentrancy guard.
    crate::e2e_state::record("settings");
}

/// Refresh the About page's live TCC status labels without reloading user settings.
/// 刷新关于页的实时 TCC 授权状态，不重载用户设置。
unsafe fn refresh_permission_statuses(ui: &SettingsUi) {
    if !ui.accessibility_permission_status.is_null()
        || !ui.accessibility_permission_button.is_null()
    {
        refresh_accessibility_permission_action(ui);
    }
    if !ui.screen_recording_permission_status.is_null() {
        set_permission_status(
            ui.screen_recording_permission_status,
            crate::thumbnail::capture_allowed(),
        );
    }
}

unsafe fn set_permission_status(label: *mut AnyObject, granted: bool) {
    let (key, color): (&str, *mut AnyObject) = if granted {
        (
            "settings.permission_status_granted",
            msg_send![class!(NSColor), systemGreenColor],
        )
    } else {
        (
            "settings.permission_status_missing",
            msg_send![class!(NSColor), systemOrangeColor],
        )
    };
    let _: () = msg_send![label, setTextColor: color];
    set_field(label, t(key));
}

unsafe fn refresh_accessibility_permission_action(ui: &SettingsUi) {
    let granted = has_accessibility_permission();
    let restart_required = granted && crate::restart::restart_required();
    if !ui.accessibility_permission_status.is_null() {
        if restart_required {
            let color: *mut AnyObject = msg_send![class!(NSColor), systemOrangeColor];
            let _: () = msg_send![ui.accessibility_permission_status, setTextColor: color];
            set_field(
                ui.accessibility_permission_status,
                t("settings.permission_status_restart_required"),
            );
        } else {
            set_permission_status(ui.accessibility_permission_status, granted);
        }
    }
    if !ui.accessibility_permission_button.is_null() {
        let (title, action) = if restart_required {
            (
                t("settings.btn_restart_to_restore_shortcuts"),
                sel!(handleRestartForAccessibility:),
            )
        } else {
            (
                t("settings.btn_open_permission_settings"),
                sel!(handleOpenPrivacy:),
            )
        };
        let title = make_nsstring(&title);
        let _: () = msg_send![ui.accessibility_permission_button, setTitle: title];
        let _: () = msg_send![ui.accessibility_permission_button, setAction: action];
        CFRelease(title as *const c_void);
    }
}

/// Refresh the visible permission UI when the app regains focus.
/// 应用重新获得焦点时刷新当前可见页面的授权状态。
pub(crate) fn refresh_permission_ui_if_visible() {
    let selected_page = SIDEBAR_SELECTED.load(Ordering::SeqCst);
    if selected_page != 0 && selected_page != 6 {
        return;
    }
    with_settings_ui(|ui| {
        if let Some(ui) = ui.as_ref() {
            unsafe {
                let visible: bool = msg_send![ui.window, isVisible];
                if visible {
                    if selected_page == 0 {
                        let is_permission_migration =
                            crate::update_notice::needs_permission_migration_copy();
                        let has_required_permissions = has_accessibility_permission()
                            && (!is_permission_migration || crate::thumbnail::capture_allowed());
                        set_permission_banner_visible(ui, !has_required_permissions);
                    } else {
                        refresh_permission_statuses(ui);
                    }
                }
            }
        }
    });
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

/// 按真实内容重新收紧 7 个页面的文档高度,返回是否有页面真的变了。
/// Re-tighten the seven page documents to their real content; returns whether any page changed.
///
/// 构建时已经收紧过一次,但条件行(跟着开关/权限显隐)和权限横幅是在窗口**显示之后**才真正
/// 展开的:那一刻页面内容会变高(实测剪贴板页 +62pt,正好一行),不重跑就会把那 62pt 又留成
/// 内容下方的死空白。
/// The build tightens once, but conditional rows (their visibility follows switches and permissions)
/// and the permission banner only expand after the window is *on screen*: the page content then gets
/// taller (measured: +62pt on the clipboard page, exactly one row), and without a second pass that
/// 62pt turns back into dead space below the content.
pub(crate) unsafe fn tighten_page_documents() -> bool {
    let mut changed = false;
    with_settings_ui(|ui| {
        let Some(ui) = ui.as_ref() else {
            return;
        };
        for scroll in [
            ui.general_view,
            ui.switcher_view,
            ui.mouse_view,
            ui.clipboard_view,
            ui.window_control_view,
            ui.quick_actions_view,
            ui.about_view,
        ] {
            if scroll.is_null() {
                continue;
            }
            let document: *mut AnyObject = msg_send![scroll, documentView];
            let clip: *mut AnyObject = msg_send![scroll, contentView];
            if document.is_null() || clip.is_null() {
                continue;
            }
            let clip_bounds: NSRect = msg_send![clip, bounds];
            let before: NSRect = msg_send![document, frame];
            let after = widgets::fit_page_document_height(
                document,
                clip_bounds.size.height,
                crate::settings::components::SettingsPageHeader::BOTTOM_PADDING,
            );
            if (after - before.size.height).abs() > 0.5 {
                changed = true;
            }
        }
    });
    changed
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
                // 迁移提示同时要求两项权限;普通提示仍只跟随辅助功能权限。
                // The migration copy requires both permissions; the regular copy follows Accessibility only.
                let is_permission_migration =
                    crate::update_notice::needs_permission_migration_copy();
                let has_required_permissions = has_accessibility_permission()
                    && (!is_permission_migration || crate::thumbnail::capture_allowed());
                let show_permission_banner =
                    SIDEBAR_SELECTED.load(Ordering::SeqCst) == 0 && !has_required_permissions;
                set_permission_banner_visible(u, show_permission_banner);
            }
        });
    }
    // 条件行/权限横幅展开后内容高度会变,再收紧一次;高度变了就说明刚才设的滚动偏移已失效,
    // 把当前页重新贴顶。
    // Conditional rows / the permission banner change the content height, so tighten once more; if the
    // height changed, the scroll offset set above is stale, so park the current page back at the top.
    unsafe {
        if tighten_page_documents() {
            let selected = SIDEBAR_SELECTED.load(Ordering::SeqCst).min(6);
            with_settings_ui(|ui| {
                if let Some(u) = ui.as_ref() {
                    let scrolls = [
                        u.general_view,
                        u.switcher_view,
                        u.mouse_view,
                        u.clipboard_view,
                        u.window_control_view,
                        u.quick_actions_view,
                        u.about_view,
                    ];
                    widgets::scroll_page_to_top(scrolls[selected]);
                }
            });
        }
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

/// Rebuild settings content and verify that configured switcher values survive the rebuild.
/// 重建设置页内容，并验证切换器配置值在重建后仍然保留。
pub(crate) fn settings_state_sync_smoke_runner() -> bool {
    unsafe {
        let mut cfg = CONFIG.read().unwrap().clone();
        cfg.windows.enabled = true;
        cfg.windows.show_minimized = true;
        cfg.layout.thumbnails_enabled = true;
        cfg.layout.focused_thumbnail_prewarm = true;
        cfg.layout.show_app_name_in_cards = true;
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg.clone();
        }

        show_settings();
        rebuild_settings_content_now();

        let states = with_settings_ui(|ui| {
            let Some(ui) = ui.as_ref() else {
                return None;
            };
            Some((
                msg_send![ui.windows_enabled, state],
                msg_send![ui.show_minimized, state],
                msg_send![ui.thumbnails_enabled, indexOfSelectedItem],
                msg_send![ui.focused_thumbnail_prewarm, state],
                msg_send![ui.show_app_name_in_cards, state],
            ))
        });
        hide_settings();

        states == Some((1isize, 1isize, 1isize, 1isize, 1isize))
    }
}

/// Verify that collapsing the clipboard child row leaves adjacent controls correctly laid out.
/// 验证收起剪贴板子行后相邻控件仍保持正确布局。
pub(crate) fn settings_collapsible_row_smoke_runner() -> bool {
    unsafe {
        let mut cfg = CONFIG.read().unwrap().clone();
        cfg.clipboard.enabled = true;
        cfg.clipboard.delete_after_paste = true;
        for profile in &mut cfg.mouse.profiles {
            profile.scroll_mode = Some("line".into());
            let pointer = profile.pointer.get_or_insert_with(Default::default);
            pointer.disable_acceleration = Some(true);
        }
        if let Ok(mut current) = CONFIG.write() {
            *current = cfg;
        }

        show_settings();
        let before = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            Some((previous, next, auto_expire))
        });
        let Some((previous_before, next_before, auto_before)) = before else {
            hide_settings();
            return false;
        };

        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                let _: () = msg_send![ui.clipboard_delete_after_paste, setState: 0isize];
                ui.clipboard_delete_block.set_visible(false);
                let _: () = msg_send![ui.window, layoutIfNeeded];
            }
        });
        let collapsed = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let child_hidden: bool =
                msg_send![ui.clipboard_clear_system_pasteboard_after_paste, isHidden];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            Some((previous, child_hidden, next, auto_expire))
        });
        let Some((previous_after, child_hidden, next_after, auto_after)) = collapsed else {
            hide_settings();
            return false;
        };

        let moved_by = next_after.origin.y - next_before.origin.y;
        let auto_moved_by = auto_after.origin.y - auto_before.origin.y;
        let expected_shift = SettingsLayout::new(400.0).row_gap + SettingsLayout::SINGLE_LINE_ROW_H;
        let stable_previous = previous_before.origin.x == previous_after.origin.x
            && previous_before.origin.y == previous_after.origin.y
            && previous_before.size.width == previous_after.size.width
            && previous_before.size.height == previous_after.size.height;
        let collapsed_ok = child_hidden
            && stable_previous
            && moved_by.abs() > 1.0
            && (moved_by.abs() - expected_shift).abs() < 1.0
            && (auto_moved_by - moved_by).abs() < 1.0;
        if !collapsed_ok {
            eprintln!(
                "[smoke-settings-collapsible-row] hidden={child_hidden} previous_stable={stable_previous} previous_y={:.1}->{:.1} next_y={:.1}->{:.1} move={moved_by:.1} auto_move={auto_moved_by:.1} expected_shift={expected_shift:.1}",
                previous_before.origin.y,
                previous_after.origin.y,
                next_before.origin.y,
                next_after.origin.y,
            );
        }

        let fitted_after_collapse = tighten_page_documents();
        let after_fit = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            Some((previous, next, auto_expire))
        });
        let Some((previous_after_fit, next_after_fit, auto_after_fit)) = after_fit else {
            hide_settings();
            return false;
        };

        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                let _: () = msg_send![ui.clipboard_delete_after_paste, setState: 1isize];
                ui.clipboard_delete_block.set_visible(true);
            }
        });
        let expanded = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let previous: NSRect = msg_send![ui.clipboard_delete_after_paste, frame];
            let next: NSRect = msg_send![ui.clipboard_max_entries, frame];
            let auto_expire: NSRect = msg_send![ui.clipboard_auto_expire_days, frame];
            Some((previous, next, auto_expire))
        });
        let fit_shift = previous_after_fit.origin.y - previous_before.origin.y;
        let restored = expanded.is_some_and(|(previous, next, auto_expire)| {
            fitted_after_collapse
                && (previous.origin.y - previous_after_fit.origin.y).abs() < 1.0
                && (next.origin.y - (next_before.origin.y + fit_shift)).abs() < 1.0
                && (auto_expire.origin.y - (auto_before.origin.y + fit_shift)).abs() < 1.0
                && (next_after_fit.origin.y - (next_before.origin.y + expected_shift + fit_shift))
                    .abs()
                    < 1.0
                && (auto_after_fit.origin.y - (auto_before.origin.y + expected_shift + fit_shift))
                    .abs()
                    < 1.0
        });
        if !restored {
            eprintln!(
                "[smoke-settings-collapsible-row] reopen after page fit failed: fit={fitted_after_collapse} fit_shift={fit_shift:.1}"
            );
        }

        // Two conditional blocks share the Mouse page. Collapsing both and reopening only the
        // upper block must not restore the lower card's old expanded frame.
        select_sidebar(2);
        let mouse_before = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
            let document_frame: NSRect = msg_send![document, frame];
            Some((
                ui.pointer_accel_block.card_frame(),
                document_frame.size.height,
            ))
        });
        let Some((pointer_expanded_frame, mouse_document_height)) = mouse_before else {
            hide_settings();
            return false;
        };
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.line_count_block.set_visible(false);
                widgets::refit_settings_page(ui.mouse_view);
                ui.pointer_accel_block.set_visible(false);
                widgets::refit_settings_page(ui.mouse_view);
            }
        });
        let pointer_collapsed_height = with_settings_ui(|ui| {
            ui.as_ref()
                .map(|ui| ui.pointer_accel_block.card_frame().size.height)
        });
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.line_count_block.set_visible(true);
                widgets::refit_settings_page(ui.mouse_view);
            }
        });
        let pointer_after_upper_expansion = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
            let document_frame: NSRect = msg_send![document, frame];
            Some((
                ui.pointer_accel_block.card_frame(),
                msg_send![ui.pointer_accel_label, isHidden],
                document_frame.size.height,
            ))
        });
        let mouse_interleaving_ok = match (pointer_collapsed_height, pointer_after_upper_expansion)
        {
            (Some(collapsed_height), Some((after_upper, pointer_hidden, _))) => {
                let row_height = pointer_expanded_frame.size.height - collapsed_height;
                (after_upper.size.height - collapsed_height).abs() < 1.0
                    && pointer_hidden
                    && (after_upper.origin.y - (pointer_expanded_frame.origin.y + row_height)).abs()
                        < 1.0
            }
            _ => false,
        };
        with_settings_ui(|ui| {
            if let Some(ui) = ui.as_ref() {
                ui.pointer_accel_block.set_visible(true);
                widgets::refit_settings_page(ui.mouse_view);
            }
        });
        let mouse_document_restored = with_settings_ui(|ui| {
            let ui = ui.as_ref()?;
            let document: *mut AnyObject = msg_send![ui.mouse_view, documentView];
            let document_frame: NSRect = msg_send![document, frame];
            Some(
                (document_frame.size.height - mouse_document_height).abs() < 1.0
                    && (ui.pointer_accel_block.card_frame().size.height
                        - pointer_expanded_frame.size.height)
                        .abs()
                        < 1.0,
            )
        });
        if !mouse_interleaving_ok || mouse_document_restored != Some(true) {
            eprintln!(
                "[smoke-settings-collapsible-row] mouse interleaving failed: interleaving={mouse_interleaving_ok} document_restored={mouse_document_restored:?}"
            );
        }
        hide_settings();
        collapsed_ok && restored && mouse_interleaving_ok && mouse_document_restored == Some(true)
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

/// 重建设置内容(滚动条样式变化后按新的可视宽度重排)。
/// Rebuilds the settings content (after a scroller-style change, to fit the new visible width).
pub(crate) unsafe fn rebuild_settings_content_now() {
    if let Some(window) = with_settings_ui(|ui| ui.as_ref().map(|ui| ui.window)) {
        rebuild_settings_content(window);
        // Rebuilding creates fresh controls, so restore the current configuration afterward.
        // 重建会创建新的控件，因此要在重建后重新填充当前配置。
        load_settings_values();
    }
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
            accessibility_permission_status: std::ptr::null_mut(),
            accessibility_permission_button: std::ptr::null_mut(),
            screen_recording_permission_status: std::ptr::null_mut(),
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
            permission_warning_view: std::ptr::null_mut(),
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
        // A:预留**实测**的滚动条占位(overlay 时为 0)。系统若强行 legacy 且 B 的重申无效,
        // 这里保证内容按可视宽度排版——只会收窄,不会被裁掉。
        // A: reserve the *measured* scroller footprint (0 for overlay). If the system forces legacy
        // and B's re-assert does not stick, this keeps the content laid out to the visible width:
        // it merely narrows instead of being clipped.
        let content_w = (detail_w - crate::scroller::reserved() - page_inset * 2.0).max(1.0);
        let layout = SettingsLayout::new(content_w);

        let target = match *MENU_TARGET.lock().unwrap() {
            Some(t) => t.0,
            None => return,
        };

        sidebar::build_settings_sidebar(
            content,
            content_h,
            view_w,
            card_margin,
            card_w,
            card_radius,
            palette,
            target,
            &mut ui,
        );

        // The scroll view spans from the left gutter to the detail pane's right edge, so the
        // overlay scrollbar sits flush with the window edge (matching the OK/Cancel footer) and
        // no longer floats 32pt in from the right. The content keeps its 32pt gutter margins
        // inside the document, so only the scrollbar's position changes.
        let page_frame = NSRect::new(
            NSPoint::new(page_x, 0.0),
            NSSize::new(detail_w - page_inset, page_viewport_h),
        );
        let page_context = page_builder::SettingsPageBuildContext::new(
            content,
            content_w,
            page_x,
            page_frame,
            page_viewport_h,
            palette,
            layout,
            target,
        );
        let content = page_context.content;
        let page_frame = page_context.page_frame;
        let _ = page_context.palette;
        let target = page_context.target;
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
        ui.about_view = about_root;
        // 记录本页 scroll view 的实测占位(供下一次构建预留;0 = overlay)。
        // Record this page's measured footprint for the next build (0 = overlay).
        crate::scroller::note_reserved(crate::scroller::reserved_width(about_root));
        let about_view = about_page.document;
        let general_content_bottom =
            page_builder::build_general_page(&page_context, general_view, general_doc_h, &mut ui);

        let keyboard_card_bottom = page_builder::build_switcher_page(
            &page_context,
            switcher_view,
            switcher_doc_h,
            &mut ui,
        );

        let mouse_content_bottom =
            page_builder::build_mouse_page(&page_context, mouse_view, mouse_doc_h, &mut ui);

        let clipboard_options_card_bottom = page_builder::build_clipboard_page(
            &page_context,
            clipboard_view,
            clipboard_doc_h,
            &mut ui,
        );

        let window_control_shortcuts_card_bottom = page_builder::build_window_control_page(
            &page_context,
            window_control_view,
            window_control_doc_h,
            &mut ui,
        );

        let quick_actions_card_bottom = page_builder::build_quick_actions_page(
            &page_context,
            quick_actions_view,
            quick_actions_doc_h,
            &mut ui,
        );

        let compact_card_bottom =
            page_builder::build_about_page(&page_context, about_view, about_doc_h, window, &mut ui);

        page_builder::finalize_settings_pages(
            content,
            window,
            page_frame,
            content_w,
            target,
            [
                general_root,
                switcher_root,
                mouse_root,
                clipboard_root,
                window_control_root,
                quick_actions_root,
                about_root,
            ],
            [
                general_view,
                switcher_view,
                mouse_view,
                clipboard_view,
                window_control_view,
                quick_actions_view,
                about_view,
            ],
            [
                general_content_bottom,
                keyboard_card_bottom,
                mouse_content_bottom,
                clipboard_options_card_bottom,
                window_control_shortcuts_card_bottom,
                quick_actions_card_bottom,
                compact_card_bottom,
            ],
            &mut ui,
        );

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

        // 窗口是带着"某个已选页"构建的(SIDEBAR_SELECTED ≠ 0,例如启动参数直接打开「关于」、
        // 或点更新通知进「关于」):页面按它建好了,但侧栏高亮默认停在第 0 项。构建**完全结束后**
        // 再应用一次选中态。注意必须在 with_settings_ui 闭包之外调用——闭包内再借一次会被
        // MainThreadSlot 的重入保护挡掉,静默什么都不做(实测高亮不动就是踩了这个)。
        // The window may be built while a page is already selected (SIDEBAR_SELECTED != 0, e.g.
        // launched straight onto About, or opened from the update notification): the page is built
        // accordingly, but the sidebar highlight defaults to item 0. Re-apply the selection once the
        // build has finished. It must run OUTSIDE the with_settings_ui closure: re-borrowing from
        // inside hits MainThreadSlot's reentrancy guard and silently does nothing (measured: that is
        // why the highlight did not move).
        select_sidebar(SIDEBAR_SELECTED.load(Ordering::SeqCst));
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
