//! 设置窗口的语义组件层：页面、卡片、行和控件布局指标。
//! Semantic components for the Settings window: pages, cards, rows, and layout metrics.
//!
//! These components intentionally remain thin wrappers around the existing AppKit builders.
//! Keeping ownership of raw Objective-C pointers in `settings.rs` avoids changing callback and
//! configuration lifetimes while giving every page one place for shared geometry rules.
//! 这里的组件刻意保持轻量，底层仍复用现有 AppKit builder。裸 Objective-C 指针的所有权继续由
//! settings.rs 管理，避免改变回调/配置生命周期，同时让所有页面共享同一套几何规则。

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::NSRect;
use std::sync::{LazyLock, Mutex};

use crate::i18n::t;

use super::{tooltip::SettingsTooltip, widgets};

/// Standard rows keep their label and control as sibling views in the card, so retain the
/// association here instead of forcing every SettingsUi field to grow a second label pointer.
/// 标准 row 的标题和控件是卡片里的兄弟 view；在组件层记录关联，避免 SettingsUi 为每个控件
/// 再增加一个 label 指针。
static ROW_LABELS: LazyLock<Mutex<Vec<(usize, usize)>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Shared horizontal and vertical metrics for all settings pages.
/// 所有设置页共享的水平/垂直布局指标。
#[derive(Clone, Copy, Debug)]
pub(super) struct SettingsLayout {
    pub label_x: f64,
    pub label_w: f64,
    pub control_x: f64,
    pub control_w: f64,
    pub row_h: f64,
    pub described_row_h: f64,
    /// Distance from one section cursor to the next section header cursor.
    /// 相邻区块标题游标之间的距离。
    pub section_step: f64,
    /// Vertical gap between rows inside a grouped card.
    /// 分组卡片内部行与行之间的垂直间距。
    pub row_gap: f64,
    /// Legacy page frame inset below the last row; normalized by SettingsSection.
    /// 页面 frame 在最后一行下方的原始内缩；由 SettingsSection 统一归一化。
    pub card_bottom_inset: f64,
    /// Gap between a section header and the card top edge.
    /// 区块标题与卡片顶部之间的间距。
    pub card_header_gap: f64,
    /// Symmetric padding for a standalone card with no section header.
    /// 没有区块标题的独立卡片使用的对称内边距。
    pub card_padding: f64,
}

impl SettingsLayout {
    /// Standard visual height of one settings row; controls remain shorter and center inside it.
    /// 设置行的标准视觉高度；控件保持更矮并在其中居中。
    pub(super) const SINGLE_LINE_ROW_H: f64 = 54.0;
    pub(super) const CONTROL_H: f64 = 34.0;

    pub(super) fn new(content_w: f64) -> Self {
        let control_w = 200.0;
        Self {
            label_x: 12.0,
            label_w: 220.0,
            control_x: content_w - control_w - super::SETTINGS_CONTROL_TRAILING_INSET,
            control_w,
            row_h: Self::CONTROL_H,
            // Detailed subtitles are no longer rendered; described rows use the same compact
            // height as every other single-line row.
            // 详细说明已不再渲染；described 行与其他单行 row 统一使用紧凑高度。
            described_row_h: Self::SINGLE_LINE_ROW_H,
            section_step: super::SETTINGS_SECTION_HEADER_GAP + 24.0,
            row_gap: 8.0,
            card_bottom_inset: 10.0,
            card_header_gap: super::SETTINGS_SECTION_CARD_GAP,
            card_padding: 4.0,
        }
    }

    pub(super) fn next_section_cursor(self, cursor: f64) -> f64 {
        cursor - self.section_step
    }

    pub(super) fn next_row_cursor(self, cursor: f64, row_h: f64) -> f64 {
        cursor - self.row_gap - row_h
    }

    pub(super) fn next_row_cursor_with_extra(self, cursor: f64, row_h: f64, extra_gap: f64) -> f64 {
        cursor - extra_gap - self.row_gap - row_h
    }

    pub(super) fn card_bottom(self, row_y: f64) -> f64 {
        row_y - self.card_bottom_inset
    }

    pub(super) fn card_top(self, header_y: f64) -> f64 {
        header_y - self.card_header_gap
    }
}

/// Ownership-neutral page parts returned by the AppKit page builder.
/// AppKit 页面 builder 返回的、不负责释放对象的页面部件。
#[derive(Clone, Copy)]
pub(super) struct SettingsPage {
    pub(super) scroll: *mut AnyObject,
    pub(super) document: *mut AnyObject,
}

unsafe impl Send for SettingsPage {}
unsafe impl Sync for SettingsPage {}

impl SettingsPage {
    pub(super) unsafe fn new(
        parent: *mut AnyObject,
        frame: NSRect,
        document_h: f64,
        hidden: bool,
    ) -> Self {
        let (scroll, document) = widgets::make_settings_page(parent, frame, document_h, hidden);
        Self { scroll, document }
    }

    pub(super) unsafe fn scroll_to_top(self) {
        widgets::scroll_page_to_top(self.scroll);
    }

    pub(super) unsafe fn validate(self, name: &str) {
        let actual: *mut AnyObject = objc2::msg_send![self.scroll, documentView];
        debug_assert_eq!(actual, self.document);
        widgets::debug_validate_settings_page(self.scroll, name);
    }
}

/// Card component. Rows remain siblings of the card background so native controls keep their
/// normal hit-testing and z-order; the component owns only the card/shadow pair.
/// 卡片组件。行仍作为卡片背景的 sibling，保证原生控件的命中和层级正常；组件只拥有卡片/阴影对。
#[derive(Clone, Copy)]
pub(super) struct SettingsCard {
    pub(super) card: *mut AnyObject,
    pub(super) shadow: *mut AnyObject,
}

unsafe impl Send for SettingsCard {}
unsafe impl Sync for SettingsCard {}

impl SettingsCard {
    pub(super) unsafe fn attach(parent: *mut AnyObject, frame: NSRect) -> Self {
        let (card, shadow) = widgets::add_settings_card(parent, frame);
        Self { card, shadow }
    }
}

/// A titled settings section: the small explanatory heading and its rounded card are one unit.
/// 带标题的设置区块：左上角说明性小标题与圆角卡片作为一个组件单元。
pub(super) struct SettingsSection;

impl SettingsSection {
    // Existing page coordinates reserve 4pt above a section card and 10pt below its last row.
    // Trim the extra bottom inset here so the row content is centered in the card's visible area
    // without requiring every page to carry a separate y-offset correction.
    // 现有页面坐标在区块卡片上方预留 4pt、最后一行下方预留 10pt。组件统一裁掉多出的底部
    // 6pt，让行内容在卡片可见区域内居中，页面调用方无需各自修正 y 坐标。
    const EXTRA_BOTTOM_INSET: f64 = 6.0;

    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        frame: NSRect,
        title: &str,
    ) -> SettingsCard {
        let header_y = frame.origin.y + frame.size.height + super::SETTINGS_SECTION_CARD_GAP;
        widgets::add_header(parent, title, 6.0, header_y, frame.size.width);
        let mut card_frame = frame;
        card_frame.origin.y += Self::EXTRA_BOTTOM_INSET;
        card_frame.size.height = (card_frame.size.height - Self::EXTRA_BOTTOM_INSET).max(1.0);
        SettingsCard::attach(parent, card_frame)
    }
}

/// Semantic row entry points. Every row centers its leading text and trailing control internally;
/// described rows retain their legacy subtitle parameter only for call-site compatibility.
/// 语义化 row 入口。每行内部统一居中左侧文字和右侧控件；described 行保留旧 subtitle 参数，
/// 仅用于兼容现有调用点，不再渲染详细说明。
pub(super) struct SettingsRow;

impl SettingsRow {
    unsafe fn register_label(label: *mut AnyObject, control: *mut AnyObject) {
        if label.is_null() || control.is_null() {
            return;
        }
        let mut labels = ROW_LABELS.lock().unwrap();
        let control = control as usize;
        labels.retain(|(registered_control, _)| *registered_control != control);
        labels.push((control, label as usize));
    }

    /// Drop row label associations before the settings views are deallocated.
    /// 设置 view 释放前清理 row 标题关联。
    pub(super) fn clear_runtime_registry() {
        ROW_LABELS.lock().unwrap().clear();
    }

    /// Apply enabled state, disabled appearance, cursor, and optional tooltip to one view.
    /// 对单个 view 同时应用启用状态、禁用外观、指针和可选 Tooltip。
    pub(super) unsafe fn set_view_enabled_with_tooltip(
        view: *mut AnyObject,
        enabled: bool,
        tooltip: Option<&str>,
    ) {
        if view.is_null() {
            return;
        }
        if objc2::msg_send![view, respondsToSelector: objc2::sel!(setEnabled:)] {
            let _: () = objc2::msg_send![view, setEnabled: enabled];
        }

        let is_text_field: bool = objc2::msg_send![view, isKindOfClass: objc2::class!(NSTextField)];
        if is_text_field {
            let color = if enabled {
                crate::ffi::hex_to_ns_color(crate::theme::ui_palette().primary_text)
            } else {
                crate::ffi::hex_to_ns_color(crate::theme::ui_palette().muted_text)
            };
            let _: () = objc2::msg_send![view, setTextColor: color];
        } else if !objc2::msg_send![view, respondsToSelector: objc2::sel!(setEnabled:)]
            && objc2::msg_send![view, respondsToSelector: objc2::sel!(setAlphaValue:)]
        {
            // NSImageView and other decorative views have no enabled property; dim them through
            // alpha so custom rows still communicate that they are unavailable.
            // NSImageView 等装饰 view 没有 enabled 属性；通过透明度置灰自定义行。
            let _: () = objc2::msg_send![view, setAlphaValue: if enabled { 1.0 } else { 0.45 }];
        }
        SettingsTooltip::apply(view, enabled, (!enabled).then_some(tooltip).flatten());
    }

    /// Enable/disable a row and show a native AppKit bubble while it is unavailable.
    /// 启用/禁用 row；不可用时显示 AppKit 原生小气泡提示。
    pub(super) unsafe fn set_enabled_with_tooltip(
        control: *mut AnyObject,
        enabled: bool,
        tooltip: &str,
    ) {
        Self::set_view_enabled_with_tooltip(control, enabled, Some(tooltip));
        if control.is_null() {
            return;
        }
        let label = ROW_LABELS
            .lock()
            .unwrap()
            .iter()
            .find(|(registered_control, _)| *registered_control == control as usize)
            .map(|(_, label)| *label as *mut AnyObject);
        if let Some(label) = label {
            Self::set_view_enabled_with_tooltip(label, enabled, Some(tooltip));
        }
    }

    /// Enable/disable a standard row as one semantic component, including its leading label.
    /// 以语义组件为单位启用/禁用标准 row，同时处理左侧标题。
    pub(super) unsafe fn set_enabled(control: *mut AnyObject, enabled: bool) {
        Self::set_enabled_with_tooltip(control, enabled, "");
    }

    /// Add the standard grouped-card divider through the same row component API.
    /// 通过统一的 row 组件 API 添加分组卡片分割线。
    pub(super) unsafe fn separator(parent: *mut AnyObject, y: f64, width: f64) -> *mut AnyObject {
        widgets::add_row_separator(parent, 0.0, y, width)
    }

    /// Center a native control by its view frame.
    /// 按控件 view frame 在 row 内垂直居中。
    unsafe fn center_control(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let mut frame: NSRect = objc2::msg_send![child, frame];
        frame.origin.y = y + (row_h - frame.size.height).max(0.0) / 2.0;
        let _: () = objc2::msg_send![child, setFrame: frame];
    }

    /// NSTextField's glyphs are top-biased when its frame is taller than the measured cell.
    /// Fit the frame to the cell's measured height before centering it, so the glyph baseline
    /// shares the same center line as the trailing control.
    /// NSTextField 在较高 frame 中会把字形偏上绘制。先收紧到 cell 实际高度，再居中 frame，
    /// 让字形基线与右侧控件共享同一条中心线。
    unsafe fn center_label(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let cell: *mut AnyObject = objc2::msg_send![child, cell];
        if cell.is_null() {
            return;
        }
        let bounds: NSRect = objc2::msg_send![child, bounds];
        let measured: objc2_foundation::NSSize = objc2::msg_send![cell, cellSizeForBounds: bounds];
        if !measured.height.is_finite() || measured.height <= 0.0 {
            return;
        }
        let mut frame: NSRect = objc2::msg_send![child, frame];
        frame.origin.y = y + (row_h - measured.height).max(0.0) / 2.0;
        frame.size.height = measured.height;
        let _: () = objc2::msg_send![child, setFrame: frame];
    }

    /// Center a read-only text control without changing editable field geometry.
    /// 只对只读文本控件收紧字形 frame；可编辑输入框保留完整控件高度。
    unsafe fn center_readonly_text_control(child: *mut AnyObject, y: f64, row_h: f64) {
        if child.is_null() {
            return;
        }
        let is_text_field: bool =
            objc2::msg_send![child, isKindOfClass: objc2::class!(NSTextField)];
        if !is_text_field {
            return;
        }
        let editable: bool = objc2::msg_send![child, isEditable];
        if !editable {
            Self::center_label(child, y, row_h);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn described(
        parent: *mut AnyObject,
        x: f64,
        y: f64,
        text_w: f64,
        row_h: f64,
        title: &str,
        subtitle: &str,
        control: *mut AnyObject,
    ) -> *mut AnyObject {
        let (label, control) =
            widgets::add_described_row(parent, x, y, text_w, row_h, title, subtitle, control);
        Self::center_label(label, y, row_h);
        Self::center_control(control, y, row_h);
        Self::register_label(label, control);
        control
    }

    pub(super) unsafe fn tall(
        parent: *mut AnyObject,
        label_x: f64,
        y: f64,
        label_w: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> (*mut AnyObject, *mut AnyObject) {
        let (label, control) = widgets::add_tall_row(
            parent,
            label_x,
            y,
            label_w,
            SettingsLayout::SINGLE_LINE_ROW_H,
            label_text,
            control,
        );
        Self::center_label(label, y, SettingsLayout::SINGLE_LINE_ROW_H);
        Self::center_control(control, y, SettingsLayout::SINGLE_LINE_ROW_H);
        Self::register_label(label, control);
        (label, control)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn plain(
        parent: *mut AnyObject,
        label_x: f64,
        y: f64,
        label_w: f64,
        h: f64,
        label_text: &str,
        control: *mut AnyObject,
    ) -> *mut AnyObject {
        let (label, control) =
            widgets::add_row_with_label(parent, label_x, y, label_w, h, label_text, control);
        Self::center_label(label, y, h);
        Self::center_readonly_text_control(control, y, h);
        Self::center_control(control, y, h);
        Self::register_label(label, control);
        control
    }
}

/// Native controls shared by settings rows and the window chrome.
/// 设置行和窗口 chrome 共用的原生控件组件入口。
pub(super) struct SettingsControl;

impl SettingsControl {
    pub(super) unsafe fn popup(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        items: &[&str],
        selected: usize,
    ) -> *mut AnyObject {
        widgets::make_popup(x, y, w, h, items, selected)
    }

    pub(super) unsafe fn switch(right_x: f64, y: f64, h: f64, checked: bool) -> *mut AnyObject {
        widgets::make_switch(right_x, y, h, checked)
    }

    pub(super) unsafe fn slider(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        min: i64,
        max: i64,
        value: i64,
    ) -> *mut AnyObject {
        widgets::make_slider(x, y, w, h, min, max, value)
    }

    pub(super) unsafe fn text_input(x: f64, y: f64, w: f64, h: f64, value: &str) -> *mut AnyObject {
        widgets::make_text_input(x, y, w, h, value)
    }

    /// Build a non-editable value label that can be placed in a `SettingsRow`.
    /// 构造可放入 `SettingsRow` 的只读值文本。
    pub(super) unsafe fn value_label(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        value: &str,
    ) -> *mut AnyObject {
        widgets::make_value_label(x, y, w, h, value)
    }

    /// Build an external-link value control with the shared link hover/cursor behavior.
    /// 构造复用统一链接悬停/光标行为的外部链接值控件。
    pub(super) unsafe fn external_link(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        title: &str,
        tag: isize,
    ) -> *mut AnyObject {
        widgets::make_external_link(x, y, w, h, title, tag)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn sidebar(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        title: &str,
        symbol: &str,
        tag: isize,
        x: f64,
        y: f64,
        w: f64,
        icon_frame: NSRect,
        label_frame: NSRect,
    ) -> *mut AnyObject {
        widgets::make_sidebar_button(
            parent,
            target,
            title,
            symbol,
            tag,
            x,
            y,
            w,
            icon_frame,
            label_frame,
        )
    }
}

/// Shared action icon component for the action popup and mapping-list rows.
/// 动作下拉与按键映射列表共用的动作图标组件。
pub(super) struct SettingsMappingActionIcon;

impl SettingsMappingActionIcon {
    /// NSPopUpButton menu cells and standalone image views apply different optical scaling.
    /// `row_size` compensates the latter so both render at the same visible size.
    /// NSPopUpButton 菜单 cell 与独立 image view 的 optical scaling 不同；`row_size` 对后者
    /// 做补偿，使两处最终视觉尺寸一致。
    pub(super) const ROW_SIZE: f64 = 18.0;

    pub(super) fn symbol_name(action_index: usize) -> Option<&'static str> {
        super::MAPPING_ACTION_SYMBOLS.get(action_index).copied()
    }

    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        action_index: usize,
        frame: NSRect,
    ) -> *mut AnyObject {
        let Some(symbol) = Self::symbol_name(action_index) else {
            return std::ptr::null_mut();
        };
        let icon = widgets::make_symbol_image_view(symbol, frame);
        let _: () = objc2::msg_send![parent, addSubview: icon];
        icon
    }
}

/// Semantic roles for clickable settings buttons. The low-level builder owns AppKit tracking;
/// this role selects the normal surface, text color, and hover behavior without leaking raw color
/// literals into page construction code.
/// 设置页可点击按钮的语义角色。底层 builder 负责 AppKit tracking；这里的 role 统一选择常态
/// 背景、文字颜色和 hover 行为，页面代码不再散落原始颜色值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SettingsButtonRole {
    Action,
    Compact,
    Footer,
    Primary,
}

impl SettingsButtonRole {
    fn style(self) -> (u32, u32, isize) {
        match self {
            // The generic action uses the translucent light surface shared by small/full actions.
            Self::Action => (0xFFFFFFAD, 0x2E2E2EFF, -3),
            // Mapping/edit actions use the denser gray surface but the same generic hover state.
            Self::Compact => (0x7676801F, 0x44444AFF, 0),
            Self::Footer => (0xFFFFFFC7, 0x2E2E2EFF, -1),
            Self::Primary => (0x0A84FFFF, 0xFFFFFFFF, -2),
        }
    }
}

/// Shared semantic button component for settings actions. Specialized controls such as toggles,
/// sidebar tabs, clipboard actions, and the overlay close button keep their own interaction model.
/// 设置页操作按钮的统一语义组件。开关、侧边栏 tab、剪贴板操作和浮窗关闭按钮拥有独立交互，
/// 继续使用各自的专用组件。
pub(super) struct SettingsButton;

impl SettingsButton {
    pub(super) unsafe fn action(
        frame: NSRect,
        title: &str,
        target: *mut AnyObject,
        action: Sel,
        role: SettingsButtonRole,
    ) -> *mut AnyObject {
        let (background, text, hover_tag) = role.style();
        widgets::make_settings_styled_button(
            frame, title, target, action, background, text, hover_tag,
        )
    }
}

/// Sidebar navigation component backed by the shared borderless button builder.
/// 侧栏导航组件，统一复用无边框按钮 builder。
pub(super) struct SettingsSidebar;

/// The icon is part of the sidebar item's semantic data, not inferred from a page tag.
/// 图标属于侧栏条目的语义数据，不再由页面 tag 隐式推断。
#[derive(Clone, Copy, Debug)]
pub(super) enum SettingsSidebarIcon {
    General,
    Switcher,
    Mouse,
    Clipboard,
    WindowControl,
    About,
}

impl SettingsSidebarIcon {
    fn symbol_name(self) -> &'static str {
        match self {
            Self::General => "gearshape",
            Self::Switcher => "rectangle.on.rectangle",
            Self::Mouse => "computermouse",
            // `doc.on.clipboard` has a dark overlapping foreground layer in the system glyph;
            // use the clean document outline so the sidebar stays visually balanced.
            // `doc.on.clipboard` 自带深色叠放前景层；改用干净的文档线框保持侧栏一致。
            Self::Clipboard => "doc.text",
            // 左右对分的矩形呼应“半屏/四分屏”的窗口控制语义。
            // A rectangle split into left/right halves mirrors the window-control
            // half-screen/quarter-snapping semantics.
            Self::WindowControl => "rectangle.split.2x1",
            Self::About => "info.circle",
        }
    }
}

fn sidebar_item_frames(w: f64) -> (NSRect, NSRect) {
    const ROW_H: f64 = 38.0;
    const ICON_X: f64 = 16.0;
    const ICON_SIZE: f64 = 18.0;
    const LABEL_X: f64 = 46.0;
    let icon_frame = NSRect::new(
        objc2_foundation::NSPoint::new(ICON_X, (ROW_H - ICON_SIZE) / 2.0),
        objc2_foundation::NSSize::new(ICON_SIZE, ICON_SIZE),
    );
    let label_frame = NSRect::new(
        objc2_foundation::NSPoint::new(LABEL_X, 0.0),
        objc2_foundation::NSSize::new((w - LABEL_X - 8.0).max(1.0), ROW_H),
    );
    (icon_frame, label_frame)
}

impl SettingsSidebar {
    pub(super) unsafe fn build(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        x: f64,
        y0: f64,
        w: f64,
    ) -> [*mut AnyObject; 6] {
        // Add new sidebar entries here: the component owns title keys, icons, tags, and spacing.
        // 新增侧栏入口只需在这里添加标题 key、图标和 tag；间距与对齐由组件统一处理。
        // 窗口控制位于剪贴板历史与关于之间。
        // Window control sits between Clipboard history and About.
        let entries = [
            ("settings.sidebar_general", SettingsSidebarIcon::General),
            ("settings.sidebar_switcher", SettingsSidebarIcon::Switcher),
            ("settings.sidebar_mouse", SettingsSidebarIcon::Mouse),
            ("settings.sidebar_clipboard", SettingsSidebarIcon::Clipboard),
            (
                "settings.sidebar_window_control",
                SettingsSidebarIcon::WindowControl,
            ),
            ("settings.sidebar_about", SettingsSidebarIcon::About),
        ];
        std::array::from_fn(|index| {
            let (title_key, icon) = entries[index];
            SettingsSidebarTab::attach(
                parent,
                target,
                &t(title_key),
                icon,
                index as isize,
                x,
                y0 - index as f64 * 42.0,
                w,
            )
        })
    }
}

/// One independently aligned icon-and-label tab inside the sidebar.
/// 侧栏中一个独立对齐的图标 + 文本 tab。
pub(super) struct SettingsSidebarTab;

impl SettingsSidebarTab {
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn attach(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        title: &str,
        icon: SettingsSidebarIcon,
        tag: isize,
        x: f64,
        y: f64,
        w: f64,
    ) -> *mut AnyObject {
        // Keep the icon and title on one explicit center line. NSTextField's cell can otherwise
        // place glyphs near the top of a 28pt frame while SF Symbols use their own optical box.
        // 用同一条明确的中心线放置图标和文字；否则 NSTextField cell 可能把字形放在 28pt
        // frame 的偏上位置，而 SF Symbols 又使用自己的 optical box，最终视觉上不对齐。
        let (icon_frame, label_frame) = sidebar_item_frames(w);
        SettingsControl::sidebar(
            parent,
            target,
            title,
            icon.symbol_name(),
            tag,
            x,
            y,
            w,
            icon_frame,
            label_frame,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{sidebar_item_frames, SettingsButtonRole, SettingsLayout};
    use crate::settings::SETTINGS_CONTROL_TRAILING_INSET;

    #[test]
    fn layout_keeps_controls_aligned_to_the_trailing_inset() {
        let layout = SettingsLayout::new(600.0);
        assert_eq!(
            layout.control_x + layout.control_w,
            600.0 - SETTINGS_CONTROL_TRAILING_INSET
        );
        assert_eq!(layout.row_h, 34.0);
        assert_eq!(layout.described_row_h, 54.0);
        assert_eq!(SettingsLayout::CONTROL_H, 34.0);
        assert_eq!(SettingsLayout::SINGLE_LINE_ROW_H, 54.0);
        assert_eq!(layout.section_step, 48.0);
        assert_eq!(layout.row_gap, 8.0);
        assert_eq!(layout.card_bottom(100.0), 90.0);
        assert_eq!(layout.card_top(100.0), 96.0);
        assert_eq!(layout.card_padding, 4.0);
        assert_eq!(layout.next_row_cursor_with_extra(100.0, 54.0, 18.0), 20.0);
    }

    #[test]
    fn sidebar_frames_share_the_same_vertical_center() {
        let (icon, label) = sidebar_item_frames(240.0);
        let icon_center = icon.origin.y + icon.size.height / 2.0;
        let label_center = label.origin.y + label.size.height / 2.0;
        assert_eq!(icon_center, label_center);
        assert_eq!(icon.origin.x, 16.0);
        assert_eq!(label.origin.x, 46.0);
    }

    #[test]
    fn button_roles_keep_normal_and_hover_semantics_distinct() {
        assert_eq!(
            SettingsButtonRole::Action.style(),
            (0xFFFFFFAD, 0x2E2E2EFF, -3)
        );
        assert_eq!(
            SettingsButtonRole::Compact.style(),
            (0x7676801F, 0x44444AFF, 0)
        );
        assert_eq!(
            SettingsButtonRole::Footer.style(),
            (0xFFFFFFC7, 0x2E2E2EFF, -1)
        );
        assert_eq!(
            SettingsButtonRole::Primary.style(),
            (0x0A84FFFF, 0xFFFFFFFF, -2)
        );
    }
}
