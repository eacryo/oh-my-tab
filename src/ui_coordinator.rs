//! Cross-surface UI coordination.
//!
//! This module owns refreshes that affect more than one UI surface. Keeping the orchestration
//! here prevents the settings implementation from reaching into menu and overlay internals.
//!
//! 跨界面 UI 协调。
//!
//! 这里集中处理会同时影响多个 UI 界面的刷新，避免设置页实现直接依赖菜单和浮窗内部细节。

/// Refresh every visible surface after a theme or locale change.
///
/// The caller must already be on the AppKit main thread. Individual surfaces continue to own
/// their controls and rendering state; this function only coordinates their public refresh APIs.
///
/// 主题或语言变化后刷新所有可见界面。
///
/// 调用方必须已经位于 AppKit 主线程；各界面仍自行拥有控件和绘制状态，本函数只协调公开刷新接口。
pub(crate) fn apply_theme_and_locale_refresh() {
    crate::menu::refresh_menu_titles();
    crate::clipboard::refresh_localized_ui();
    unsafe {
        crate::clipboard::apply_theme();
    }
    crate::overlay::apply_theme();
    crate::overlay::refresh_highlight();
    crate::overlay::update_status_label();
}
