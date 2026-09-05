//! 设置窗口的语义组件层：页面、卡片、行和控件布局指标。
//! Semantic components for the Settings window: pages, cards, rows, and layout metrics.
//!
//! These components intentionally remain thin wrappers around the existing AppKit builders.
//! Keeping ownership of raw Objective-C pointers in `settings.rs` avoids changing callback and
//! configuration lifetimes while giving every page one place for shared geometry rules.
//! 这里的组件刻意保持轻量，底层仍复用现有 AppKit builder。裸 Objective-C 指针的所有权继续由
//! settings.rs 管理，避免改变回调/配置生命周期，同时让所有页面共享同一套几何规则。

use objc2::runtime::{AnyObject, Sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::sync::atomic::Ordering;
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
            let role = if enabled {
                widgets::SettingsTextRole::Primary
            } else {
                widgets::SettingsTextRole::Disabled
            };
            widgets::apply_settings_text_role(view, role);
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

/// Animated select component shared by settings rows and auxiliary edit panels.
/// 设置行和辅助编辑面板共用的动画选择器组件。
pub(super) struct SettingsSelect;

impl SettingsSelect {
    pub(super) unsafe fn create(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        items: &[&str],
        selected: usize,
    ) -> *mut AnyObject {
        widgets::make_popup(x, y, w, h, items, selected)
    }
}

impl SettingsControl {
    pub(super) unsafe fn popup(
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        items: &[&str],
        selected: usize,
    ) -> *mut AnyObject {
        SettingsSelect::create(x, y, w, h, items, selected)
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
    Destructive,
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
            Self::Destructive => (0xFF3B30FF, 0xFFFFFFFF, -4),
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

/// Restore-defaults footer control: one compact trigger that morphs into confirm/cancel rows.
/// 恢复默认设置 footer 控件：单个紧凑触发按钮 morph 成确认/取消两行。
///
/// The control owns the view graph and animation geometry; settings.rs only coordinates the
/// business action and keeps this component in SettingsUi. This keeps the raw-pointer lifetime
/// with the settings window while making the whole lower-left control one semantic component.
/// 控件统一管理 view 层级和动画几何；settings.rs 只负责业务动作并将组件存入 SettingsUi。
/// 裸指针生命周期仍归设置窗口所有，同时让左下角控件成为一个完整语义组件。
#[derive(Clone, Copy)]
pub(super) struct RestoreDefaultsControl {
    pub(super) trigger: *mut AnyObject,
    pub(super) confirm: *mut AnyObject,
    pub(super) cancel: *mut AnyObject,
    pub(super) surface: *mut AnyObject,
    pub(super) container: *mut AnyObject,
    pub(super) separator: *mut AnyObject,
    // 展开几何(侧边栏与 footer 变体尺寸不同,构建时确定):
    // confirm_y = 确认按钮在容器内的 y;collapsed/expanded_h = 容器收起/展开高度。
    // Expanded geometry (sidebar vs footer variants differ; fixed at build time):
    // confirm_y = the confirm row's y inside the container; collapsed/expanded_h = the
    // container's collapsed/expanded height.
    confirm_y: f64,
    collapsed_h: f64,
    expanded_h: f64,
    pub(super) expanded: bool,
}

unsafe impl Send for RestoreDefaultsControl {}
unsafe impl Sync for RestoreDefaultsControl {}

impl RestoreDefaultsControl {
    pub(super) fn empty() -> Self {
        Self {
            trigger: std::ptr::null_mut(),
            confirm: std::ptr::null_mut(),
            cancel: std::ptr::null_mut(),
            surface: std::ptr::null_mut(),
            container: std::ptr::null_mut(),
            separator: std::ptr::null_mut(),
            confirm_y: 0.0,
            collapsed_h: 0.0,
            expanded_h: 0.0,
            expanded: false,
        }
    }

    pub(super) unsafe fn build(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        sidebar_width: f64,
    ) -> Self {
        let button_frame = NSRect::new(
            NSPoint::new(8.0, 6.0),
            objc2_foundation::NSSize::new(sidebar_width - 44.0, 30.0),
        );
        let separator: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let separator: *mut AnyObject = objc2::msg_send![
            separator,
            initWithFrame: NSRect::new(
                NSPoint::new(26.0, 61.0),
                objc2_foundation::NSSize::new(sidebar_width - 52.0, 1.0),
            )
        ];
        let _: () = objc2::msg_send![separator, setWantsLayer: true];
        let separator_layer: *mut AnyObject = objc2::msg_send![separator, layer];
        if !separator_layer.is_null() {
            crate::ffi::layer_set_background(
                separator_layer,
                crate::ffi::hex_to_cg_color(widgets::settings_palette().separator),
            );
        }
        let _: () = objc2::msg_send![parent, addSubview: separator];

        // The expanded card is a separate surface behind the buttons. Keeping it outside the
        // button container lets the compact trigger retain its original look while the card
        // fades/grows in as one rounded surface.
        // 展开卡片是位于按钮后方的独立表面。将它与按钮容器分开，收起时保留原按钮外观，
        // 展开时再让圆角卡片作为整体淡入并长大。
        let surface: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let surface: *mut AnyObject = objc2::msg_send![
            surface,
            initWithFrame: NSRect::new(
                NSPoint::new(22.0, 20.0),
                objc2_foundation::NSSize::new(sidebar_width - 44.0, 30.0),
            )
        ];
        let _: () = objc2::msg_send![surface, setWantsLayer: true];
        let surface_layer: *mut AnyObject = objc2::msg_send![surface, layer];
        if !surface_layer.is_null() {
            let palette = widgets::settings_palette();
            crate::ffi::layer_set_background(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_bg),
            );
            crate::ffi::layer_set_border(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_border),
            );
            let _: () = objc2::msg_send![surface_layer, setBorderWidth: 1.0f64];
            let _: () = objc2::msg_send![surface_layer, setCornerRadius: 14.0f64];
            let _: () = objc2::msg_send![surface_layer, setMasksToBounds: false];
            let _: () = objc2::msg_send![surface_layer, setShadowOpacity: 0.0f32];
            let _: () = objc2::msg_send![surface_layer, setShadowRadius: 8.0f64];
            let _: () = objc2::msg_send![surface_layer, setShadowOffset: NSSize::new(0.0, -1.0)];
        }
        // Collapsed state: the shell stays fully hidden. It shares the trigger's frame, and its
        // own hairline border composites with the trigger's border into a muddy double ring
        // (most visible on hover when the trigger fill turns translucent). It is revealed only
        // while expanding.
        // 收起态外壳完全隐藏：它与触发按钮同框，自身描边会和按钮描边叠成浊环（悬停时按钮
        // 变半透明灰后最明显），只在展开时才显示。
        let _: () = objc2::msg_send![surface, setHidden: true];
        let _: () = objc2::msg_send![surface, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![parent, addSubview: surface];

        let container: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let container: *mut AnyObject = objc2::msg_send![
            container,
            initWithFrame: NSRect::new(
                NSPoint::new(14.0, 14.0),
                objc2_foundation::NSSize::new(sidebar_width - 28.0, 42.0),
            )
        ];
        let _: () = objc2::msg_send![container, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![container, setWantsLayer: true];
        let container_layer: *mut AnyObject = objc2::msg_send![container, layer];
        if !container_layer.is_null() {
            // Match the reference root's `overflow-hidden`: the upper row is revealed only as
            // the shell grows past it.
            // 对齐参考根节点的 `overflow-hidden`：上排按钮只会在外壳长到对应高度后显现。
            let _: () = objc2::msg_send![container_layer, setMasksToBounds: true];
            let _: () = objc2::msg_send![container_layer, setCornerRadius: 14.0f64];
        }
        let _: () = objc2::msg_send![parent, addSubview: container];

        let trigger = SettingsButton::action(
            button_frame,
            &t("settings.btn_restore_defaults"),
            target,
            objc2::sel!(handleRestoreDefaults:),
            SettingsButtonRole::Action,
        );
        let _: () = objc2::msg_send![trigger, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![container, addSubview: trigger];

        let confirm = SettingsButton::action(
            button_frame,
            &t("settings.btn_confirm"),
            target,
            objc2::sel!(handleRestoreDefaultsConfirm:),
            SettingsButtonRole::Destructive,
        );
        let _: () = objc2::msg_send![confirm, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![confirm, setHidden: true];
        let _: () = objc2::msg_send![confirm, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: confirm];

        let cancel = SettingsButton::action(
            button_frame,
            &t("settings.btn_cancel"),
            target,
            objc2::sel!(handleRestoreDefaultsCancel:),
            SettingsButtonRole::Action,
        );
        let _: () = objc2::msg_send![cancel, setAutoresizingMask: 36u64];
        let _: () = objc2::msg_send![cancel, setHidden: true];
        let _: () = objc2::msg_send![cancel, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: cancel];

        crate::ffi::CFRelease(separator as *const std::ffi::c_void);
        crate::ffi::CFRelease(surface as *const std::ffi::c_void);
        crate::ffi::CFRelease(container as *const std::ffi::c_void);
        crate::ffi::CFRelease(trigger as *const std::ffi::c_void);
        crate::ffi::CFRelease(confirm as *const std::ffi::c_void);
        crate::ffi::CFRelease(cancel as *const std::ffi::c_void);

        Self {
            trigger,
            confirm,
            cancel,
            surface,
            container,
            separator,
            confirm_y: 54.0,
            collapsed_h: 42.0,
            expanded_h: 110.0,
            expanded: false,
        }
    }

    /// 页面变体:内嵌到某页文档内容末尾的「恢复本页默认设置」控件(无分割线)。
    /// Page variant: a "Restore Page Defaults" control embedded at the end of one page's
    /// scrolling document (no separator).
    ///
    /// `(x, y_bottom)` 是控件收起态容器在文档坐标系中的左下角(y 向上),`width` 为控件
    /// 全宽(与卡片同宽)。展开卡片向上生长,盖在页面内容之上(控件是文档的最后子视图)。
    /// `(x, y_bottom)` is the collapsed container's bottom-left corner in the document's
    /// coordinate space (y up); `width` is the control's full width (matching the cards). The
    /// expanded card grows upward, drawing over page content (the control is the document's
    /// last subview).
    #[allow(clippy::too_many_arguments)]
    pub(super) unsafe fn build_for_page(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        x: f64,
        y_bottom: f64,
        width: f64,
    ) -> Self {
        let row_h = 30.0;
        let container_frame = NSRect::new(
            NSPoint::new(x, y_bottom),
            objc2_foundation::NSSize::new(width, 42.0),
        );
        let container: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        // initWithFrame: 返回对象本身;objc2 在 debug 下校验返回类型编码,必须绑定返回值。
        // initWithFrame: returns the object; objc2 validates the return type encoding in debug
        // builds, so the return value must be bound.
        let container: *mut AnyObject = objc2::msg_send![container, initWithFrame: container_frame];
        // 文档宽度固定,不留自适应掩码;随内容一起滚动。
        // The document width is fixed: no autoresizing mask; it simply scrolls with the content.
        let _: () = objc2::msg_send![container, setAutoresizingMask: 0u64];
        let _: () = objc2::msg_send![container, setWantsLayer: true];
        let container_layer: *mut AnyObject = objc2::msg_send![container, layer];
        if !container_layer.is_null() {
            // 对齐参考根节点的 `overflow-hidden`:上排按钮只会在外壳长到对应高度后显现。
            // Match the reference root's `overflow-hidden`: the upper row is revealed only as
            // the shell grows past it.
            let _: () = objc2::msg_send![container_layer, setMasksToBounds: true];
            let _: () = objc2::msg_send![container_layer, setCornerRadius: 14.0f64];
        }

        // 触发按钮全宽(与卡片同宽,左右各留 8pt 内边距)。
        // The trigger is full width (matching the cards, with 8pt side insets).
        let trigger = SettingsButton::action(
            NSRect::new(
                NSPoint::new(8.0, 6.0),
                objc2_foundation::NSSize::new(width - 16.0, row_h),
            ),
            &t("settings.btn_restore_page_defaults"),
            target,
            objc2::sel!(handlePageRestoreDefaults:),
            SettingsButtonRole::Action,
        );

        // 展开卡片是位于按钮后方的独立表面,展开时作为整体淡入并向上生长。
        // The expanded card is a separate surface behind the buttons that fades in and grows
        // upward as one rounded unit.
        // 收起态外壳完全隐藏(避免与按钮描边叠成浊环,见侧边栏变体说明)。
        // Collapsed shell stays hidden (same double-ring reason as the sidebar variant).
        let surface: *mut AnyObject = objc2::msg_send![objc2::class!(NSView), alloc];
        let surface: *mut AnyObject = objc2::msg_send![surface, initWithFrame: NSRect::new(
            NSPoint::new(x + 8.0, y_bottom + 6.0),
            objc2_foundation::NSSize::new(width - 16.0, row_h),
        )];
        let _: () = objc2::msg_send![surface, setHidden: true];
        let _: () = objc2::msg_send![surface, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![surface, setWantsLayer: true];
        let surface_layer: *mut AnyObject = objc2::msg_send![surface, layer];
        if !surface_layer.is_null() {
            let palette = widgets::settings_palette();
            crate::ffi::layer_set_background(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_bg),
            );
            crate::ffi::layer_set_border(
                surface_layer,
                crate::ffi::hex_to_cg_color(palette.card_border),
            );
            let _: () = objc2::msg_send![surface_layer, setBorderWidth: 1.0f64];
            let _: () = objc2::msg_send![surface_layer, setCornerRadius: 14.0f64];
            let _: () = objc2::msg_send![surface_layer, setMasksToBounds: false];
            let _: () = objc2::msg_send![surface_layer, setShadowOpacity: 0.0f32];
            let _: () = objc2::msg_send![surface_layer, setShadowRadius: 8.0f64];
            let _: () = objc2::msg_send![surface_layer, setShadowOffset: NSSize::new(0.0, -1.0)];
        }
        let _: () = objc2::msg_send![parent, addSubview: surface];
        // container(含按钮)必须加到 surface 之上，否则按钮会被外壳盖住。
        // The container (with the buttons) must join the hierarchy ABOVE the surface.
        let _: () = objc2::msg_send![parent, addSubview: container];

        let confirm = SettingsButton::action(
            NSRect::new(
                NSPoint::new(8.0, 54.0),
                objc2_foundation::NSSize::new(width - 16.0, row_h),
            ),
            &t("settings.btn_confirm"),
            target,
            objc2::sel!(handlePageRestoreDefaultsConfirm:),
            SettingsButtonRole::Destructive,
        );
        let _: () = objc2::msg_send![confirm, setHidden: true];
        let _: () = objc2::msg_send![confirm, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: confirm];

        let cancel = SettingsButton::action(
            NSRect::new(
                NSPoint::new(8.0, 6.0),
                objc2_foundation::NSSize::new(width - 16.0, row_h),
            ),
            &t("settings.btn_cancel"),
            target,
            objc2::sel!(handlePageRestoreDefaultsCancel:),
            SettingsButtonRole::Action,
        );
        let _: () = objc2::msg_send![cancel, setHidden: true];
        let _: () = objc2::msg_send![cancel, setAlphaValue: 0.0f64];
        let _: () = objc2::msg_send![container, addSubview: cancel];

        let _: () = objc2::msg_send![container, addSubview: trigger];

        crate::ffi::CFRelease(surface as *const std::ffi::c_void);
        crate::ffi::CFRelease(container as *const std::ffi::c_void);
        crate::ffi::CFRelease(trigger as *const std::ffi::c_void);
        crate::ffi::CFRelease(confirm as *const std::ffi::c_void);
        crate::ffi::CFRelease(cancel as *const std::ffi::c_void);

        Self {
            trigger,
            confirm,
            cancel,
            surface,
            container,
            separator: std::ptr::null_mut(),
            // 取消行与触发按钮同位(y=6),确认行在其上方;展开高度 = 确认行顶 + 8pt 余量。
            // The cancel row shares the trigger's position (y=6); the confirm row sits above;
            // expanded height = the confirm row's top + an 8pt margin.
            confirm_y: 54.0,
            collapsed_h: 42.0,
            expanded_h: 110.0,
            expanded: false,
        }
    }

    pub(super) fn is_ready(self) -> bool {
        // separator 仅侧边栏变体存在(footer 变体为空),不参与就绪判定。
        // The separator only exists in the sidebar variant (null for footer); it is not part
        // of the readiness check.
        !self.trigger.is_null()
            && !self.confirm.is_null()
            && !self.cancel.is_null()
            && !self.surface.is_null()
            && !self.container.is_null()
    }

    /// Toggle the component as one animated unit; the bottom row remains anchored in place.
    /// 将整个控件作为一个动画单元切换；底部一行始终保持锚定。
    pub(super) unsafe fn set_expanded(&mut self, expanded: bool, animated: bool) {
        if !self.is_ready() || self.expanded == expanded {
            return;
        }
        self.expanded = expanded;
        let animated = animated && !Self::accessibility_reduce_motion();

        let trigger_frame: NSRect = objc2::msg_send![self.trigger, frame];
        let container_frame: NSRect = objc2::msg_send![self.container, frame];
        // The card is only slightly wider than the original trigger; both expanded rows keep the
        // trigger's original width and horizontal inset. The top padding is intentionally compact
        // so the card does not leave a large empty panel above Confirm.
        // 卡片只比原触发按钮略宽；展开后的两行继续使用原按钮宽度和水平内边距。顶部留白
        // 有意收紧，避免 Confirm 上方出现大块空白区域。
        let expanded_row_width = trigger_frame.size.width;
        let cancel_frame = NSRect::new(
            // Keep Cancel exactly where the collapsed trigger lives. The card grows upward
            // around this fixed bottom anchor.
            // 取消按钮始终与收起态的触发按钮完全同位；卡片围绕这个固定底部锚点向上展开。
            trigger_frame.origin,
            objc2_foundation::NSSize::new(expanded_row_width, trigger_frame.size.height),
        );
        let confirm_frame = NSRect::new(
            NSPoint::new(trigger_frame.origin.x, self.confirm_y),
            objc2_foundation::NSSize::new(expanded_row_width, trigger_frame.size.height),
        );
        // footer 变体没有分割线(separator 为空),跳过分割线动画。
        // The footer variant has no separator (null); skip the separator animation.
        let separator_frame: NSRect = if self.separator.is_null() {
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0))
        } else {
            objc2::msg_send![self.separator, frame]
        };
        let target_separator = if self.separator.is_null() {
            separator_frame
        } else {
            let target_separator_y = if expanded {
                separator_frame.origin.y + 51.0
            } else {
                separator_frame.origin.y - 51.0
            };
            NSRect::new(
                NSPoint::new(separator_frame.origin.x, target_separator_y),
                separator_frame.size,
            )
        };
        let target_container = NSRect::new(
            container_frame.origin,
            objc2_foundation::NSSize::new(
                container_frame.size.width,
                if expanded {
                    self.expanded_h
                } else {
                    self.collapsed_h
                },
            ),
        );
        // The shell starts exactly behind the trigger and grows outward by 8pt on each side,
        // while its bottom edge stays fixed. This also keeps the compact outline fully covered.
        // 外壳从触发按钮正后方开始，左右各向外长 8pt，同时底边保持固定；收起时轮廓会
        // 被按钮完整遮住。
        let collapsed_surface = NSRect::new(
            NSPoint::new(
                container_frame.origin.x + trigger_frame.origin.x,
                container_frame.origin.y + trigger_frame.origin.y,
            ),
            trigger_frame.size,
        );
        let target_surface = if expanded {
            target_container
        } else {
            collapsed_surface
        };

        if expanded {
            // Reveal the shell for the expanded card (it stays hidden while collapsed; see the
            // build sites).
            // 展开时先显示外壳（收起态保持隐藏，见构建处）。
            let _: () = objc2::msg_send![self.surface, setHidden: false];
            let _: () = objc2::msg_send![self.trigger, setHidden: false];
            let _: () = objc2::msg_send![self.cancel, setHidden: false];
            let _: () = objc2::msg_send![self.confirm, setHidden: false];
            if animated {
                Self::animate_basic_opacity(self.surface, 1.0, 0.25, "restore-shell-reveal");
                // 同步视图模型值,保证与后续非动画路径(setAlphaValue)读写一致。
                // Sync the view model value so later non-animated paths stay consistent.
                let _: () = objc2::msg_send![self.surface, setAlphaValue: 1.0f64];
            } else {
                let _: () = objc2::msg_send![self.surface, setAlphaValue: 1.0f64];
            }
            let _: () = objc2::msg_send![self.confirm, setFrame: confirm_frame];
            let _: () = objc2::msg_send![self.cancel, setFrame: cancel_frame];
            // Cancel takes the fixed dock slot while Restore Defaults fades beneath it.
            // 取消按钮占据固定 dock 位置，恢复默认按钮在其下方淡出。
            let _: () = objc2::msg_send![
                self.container,
                addSubview: self.cancel,
                positioned: 1isize,
                relativeTo: std::ptr::null::<AnyObject>()
            ];
            if animated {
                Self::animate_basic_opacity(self.trigger, 0.0, 0.16, "restore-trigger-close");
                Self::animate_spring_opacity(
                    self.cancel,
                    1.0,
                    0.38,
                    350.0,
                    36.0,
                    "restore-cancel-open",
                );
                Self::animate_content_open(self.confirm);
            } else {
                let _: () = objc2::msg_send![self.trigger, setAlphaValue: 0.0f64];
                let _: () = objc2::msg_send![self.cancel, setAlphaValue: 1.0f64];
                Self::set_content_model(self.confirm, 1.0, 0.0, 1.0);
            }
        } else {
            let _: () = objc2::msg_send![self.trigger, setHidden: false];
            let _: () = objc2::msg_send![self.cancel, setHidden: false];
            let _: () = objc2::msg_send![self.confirm, setHidden: false];
            // Reorder the trigger above the transparent closing controls so repeated Cancel/open
            // cycles cannot leave an invisible button swallowing clicks.
            // 将触发按钮移到正在淡出的透明控件之上，避免重复取消/展开后被不可见按钮吞掉点击。
            let _: () = objc2::msg_send![
                self.container,
                addSubview: self.trigger,
                positioned: 1isize,
                relativeTo: std::ptr::null::<AnyObject>()
            ];
            if animated {
                Self::animate_content_exit(self.confirm);
                Self::animate_basic_opacity(self.cancel, 0.0, 0.16, "restore-cancel-close");
                Self::animate_spring_opacity(
                    self.trigger,
                    1.0,
                    0.38,
                    350.0,
                    36.0,
                    "restore-trigger-open",
                );
                // Fade the shell with the shrink so it never ends up painting behind the
                // trigger in the collapsed state.
                // 外壳随收缩淡出，避免收起态结束时仍画在按钮后面。
                Self::animate_basic_opacity(self.surface, 0.0, 0.45, "restore-shell-hide");
                let _: () = objc2::msg_send![self.surface, setAlphaValue: 0.0f64];
            } else {
                let _: () = objc2::msg_send![self.trigger, setAlphaValue: 1.0f64];
                let _: () = objc2::msg_send![self.cancel, setAlphaValue: 0.0f64];
                Self::set_content_model(self.confirm, 0.0, 6.0, 0.98);
                let _: () = objc2::msg_send![self.surface, setAlphaValue: 0.0f64];
                let _: () = objc2::msg_send![self.surface, setHidden: true];
            }
        }

        if animated {
            // Exact beUI timing: shell spring 0.58s with a restrained 0.06 bounce. The native
            // stiffness/damping pair is calibrated to that low-bounce duration.
            // 严格采用 beUI 的外壳时长：0.58 秒、0.06 低回弹；原生刚度/阻尼按该曲线校准。
            Self::spring_view_frame(
                self.surface,
                target_surface,
                0.58,
                160.0,
                24.0,
                "restore-shell",
            );
            let surface_layer: *mut AnyObject = objc2::msg_send![self.surface, layer];
            if !surface_layer.is_null() {
                let target_shadow = if expanded { 0.12 } else { 0.0 };
                let from_shadow = Self::presentation_scalar(
                    surface_layer,
                    "shadowOpacity",
                    if expanded { 0.0 } else { 0.12 },
                );
                Self::animate_spring_scalar(
                    surface_layer,
                    "shadowOpacity",
                    from_shadow,
                    target_shadow,
                    0.58,
                    160.0,
                    24.0,
                    "restore-shell-shadow",
                );
            }
            Self::spring_view_frame(
                self.container,
                target_container,
                0.58,
                160.0,
                24.0,
                "restore-clip",
            );
            if !self.separator.is_null() {
                Self::spring_view_frame(
                    self.separator,
                    target_separator,
                    0.58,
                    160.0,
                    24.0,
                    "restore-divider",
                );
            }
        } else {
            let _: () = objc2::msg_send![self.surface, setFrame: target_surface];
            let surface_layer: *mut AnyObject = objc2::msg_send![self.surface, layer];
            if !surface_layer.is_null() {
                let shadow_opacity = if expanded { 0.12f32 } else { 0.0f32 };
                let _: () = objc2::msg_send![surface_layer, setShadowOpacity: shadow_opacity];
            }
            let _: () = objc2::msg_send![self.container, setFrame: target_container];
            if !self.separator.is_null() {
                let _: () = objc2::msg_send![self.separator, setFrame: target_separator];
            }
        }
    }

    unsafe fn accessibility_reduce_motion() -> bool {
        let workspace: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSWorkspace), sharedWorkspace];
        if workspace.is_null()
            || !objc2::msg_send![workspace, respondsToSelector: objc2::sel!(accessibilityDisplayShouldReduceMotion)]
        {
            return false;
        }
        objc2::msg_send![workspace, accessibilityDisplayShouldReduceMotion]
    }

    unsafe fn spring_view_frame(
        view: *mut AnyObject,
        target_frame: NSRect,
        duration: f64,
        stiffness: f64,
        damping: f64,
        key_prefix: &str,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setFrame: target_frame];
            return;
        }

        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        let from_bounds: NSRect = if presentation.is_null() {
            objc2::msg_send![layer, bounds]
        } else {
            objc2::msg_send![presentation, bounds]
        };
        let from_position: NSPoint = if presentation.is_null() {
            objc2::msg_send![layer, position]
        } else {
            objc2::msg_send![presentation, position]
        };

        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        let _: () = objc2::msg_send![view, setFrame: target_frame];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];

        let to_bounds: NSRect = objc2::msg_send![layer, bounds];
        let to_position: NSPoint = objc2::msg_send![layer, position];
        let from_bounds_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithRect: from_bounds];
        let to_bounds_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithRect: to_bounds];
        Self::add_spring_value(
            layer,
            "bounds",
            from_bounds_value,
            to_bounds_value,
            duration,
            stiffness,
            damping,
            &format!("{key_prefix}-bounds"),
        );

        let from_position_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithPoint: from_position];
        let to_position_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSValue), valueWithPoint: to_position];
        Self::add_spring_value(
            layer,
            "position",
            from_position_value,
            to_position_value,
            duration,
            stiffness,
            damping,
            &format!("{key_prefix}-position"),
        );
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn add_spring_value(
        layer: *mut AnyObject,
        key_path: &str,
        from: *mut AnyObject,
        to: *mut AnyObject,
        duration: f64,
        stiffness: f64,
        damping: f64,
        animation_key: &str,
    ) {
        let key_path = crate::ffi::make_nsstring(key_path);
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CASpringAnimation),
            animationWithKeyPath: key_path
        ];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        let _: () = objc2::msg_send![animation, setFromValue: from];
        let _: () = objc2::msg_send![animation, setToValue: to];
        let _: () = objc2::msg_send![animation, setMass: 1.0f64];
        let _: () = objc2::msg_send![animation, setStiffness: stiffness];
        let _: () = objc2::msg_send![animation, setDamping: damping];
        let _: () = objc2::msg_send![animation, setInitialVelocity: 0.0f64];
        let _: () = objc2::msg_send![animation, setDuration: duration];
        let animation_key = crate::ffi::make_nsstring(animation_key);
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(animation_key as *const std::ffi::c_void);
    }

    unsafe fn set_layer_scalar(layer: *mut AnyObject, key_path: &str, value: f64) {
        let value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: value];
        let key_path = crate::ffi::make_nsstring(key_path);
        let _: () = objc2::msg_send![layer, setValue: value, forKeyPath: key_path];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
    }

    unsafe fn presentation_scalar(layer: *mut AnyObject, key_path: &str, fallback: f64) -> f64 {
        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        if presentation.is_null() {
            return fallback;
        }
        let key_path = crate::ffi::make_nsstring(key_path);
        let value: *mut AnyObject = objc2::msg_send![presentation, valueForKeyPath: key_path];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        if value.is_null() {
            fallback
        } else {
            objc2::msg_send![value, doubleValue]
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn animate_spring_scalar(
        layer: *mut AnyObject,
        key_path: &str,
        from: f64,
        to: f64,
        duration: f64,
        stiffness: f64,
        damping: f64,
        animation_key: &str,
    ) {
        let from: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: from];
        let to_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: to];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, key_path, to);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        Self::add_spring_value(
            layer,
            key_path,
            from,
            to_value,
            duration,
            stiffness,
            damping,
            animation_key,
        );
    }

    unsafe fn animate_spring_opacity(
        view: *mut AnyObject,
        target: f64,
        duration: f64,
        stiffness: f64,
        damping: f64,
        animation_key: &str,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: target];
            return;
        }
        let fallback: f64 = objc2::msg_send![view, alphaValue];
        let from = Self::presentation_scalar(layer, "opacity", fallback);
        Self::animate_spring_scalar(
            layer,
            "opacity",
            from,
            target,
            duration,
            stiffness,
            damping,
            animation_key,
        );
    }

    unsafe fn animate_basic_opacity(
        view: *mut AnyObject,
        target: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: target];
            return;
        }
        let fallback: f64 = objc2::msg_send![view, alphaValue];
        let from = Self::presentation_scalar(layer, "opacity", fallback);
        Self::animate_basic_scalar(layer, "opacity", from, target, duration, animation_key);
    }

    unsafe fn animate_basic_scalar(
        layer: *mut AnyObject,
        key_path: &str,
        from: f64,
        to: f64,
        duration: f64,
        animation_key: &str,
    ) {
        let from_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: from];
        let to_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: to];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, key_path, to);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        let key_path = crate::ffi::make_nsstring(key_path);
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        let _: () = objc2::msg_send![animation, setFromValue: from_value];
        let _: () = objc2::msg_send![animation, setToValue: to_value];
        let _: () = objc2::msg_send![animation, setDuration: duration];
        // `functionWithControlPoints::::` has unlabeled selector segments that `msg_send!`
        // cannot express, so call it through the typed Objective-C entry point.
        // `functionWithControlPoints::::` 包含无标签 selector 段，`msg_send!` 无法表达，
        // 因此通过带类型的 Objective-C 入口调用。
        extern "C" {
            fn objc_msgSend();
        }
        type TimingFunction =
            unsafe extern "C" fn(*mut AnyObject, Sel, f32, f32, f32, f32) -> *mut AnyObject;
        let make_timing: TimingFunction = std::mem::transmute(objc_msgSend as *const ());
        let timing = make_timing(
            objc2::class!(CAMediaTimingFunction) as *const _ as *mut AnyObject,
            objc2::sel!(functionWithControlPoints::::),
            0.16,
            1.0,
            0.3,
            1.0,
        );
        if !timing.is_null() {
            let _: () = objc2::msg_send![animation, setTimingFunction: timing];
        }
        let animation_key = crate::ffi::make_nsstring(animation_key);
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(animation_key as *const std::ffi::c_void);
    }

    unsafe fn set_content_model(view: *mut AnyObject, opacity: f64, y: f64, scale: f64) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: opacity];
            return;
        }
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        Self::set_layer_scalar(layer, "opacity", opacity);
        Self::set_layer_scalar(layer, "transform.translation.y", y);
        Self::set_layer_scalar(layer, "transform.scale", scale);
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
    }

    unsafe fn animate_content_open(view: *mut AnyObject) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: 1.0f64];
            return;
        }
        Self::set_content_model(view, 1.0, 0.0, 1.0);
        // Exact CONTENT_VARIANTS + CONTENT_SPRING mapping from the reference. AppKit's positive
        // Y points upward, so CSS y:-8 maps to native y:+8.
        // 严格映射参考组件的 CONTENT_VARIANTS 与 CONTENT_SPRING。AppKit 正 Y 向上，
        // 因此 CSS y:-8 对应原生 y:+8。
        Self::animate_spring_scalar(
            layer,
            "opacity",
            0.0,
            1.0,
            0.46,
            266.0,
            30.0,
            "restore-content-opacity",
        );
        Self::animate_spring_scalar(
            layer,
            "transform.translation.y",
            8.0,
            0.0,
            0.46,
            266.0,
            30.0,
            "restore-content-y",
        );
        Self::animate_spring_scalar(
            layer,
            "transform.scale",
            0.98,
            1.0,
            0.46,
            266.0,
            30.0,
            "restore-content-scale",
        );
    }

    unsafe fn animate_content_exit(view: *mut AnyObject) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: 0.0f64];
            return;
        }
        let opacity = Self::presentation_scalar(layer, "opacity", 1.0);
        let y = Self::presentation_scalar(layer, "transform.translation.y", 0.0);
        let scale = Self::presentation_scalar(layer, "transform.scale", 1.0);
        Self::animate_basic_scalar(
            layer,
            "opacity",
            opacity,
            0.0,
            0.08,
            "restore-content-opacity",
        );
        Self::animate_basic_scalar(
            layer,
            "transform.translation.y",
            y,
            6.0,
            0.08,
            "restore-content-y",
        );
        Self::animate_basic_scalar(
            layer,
            "transform.scale",
            scale,
            0.98,
            0.08,
            "restore-content-scale",
        );
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
    QuickActions,
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
            Self::WindowControl => "rectangle.split.2x2",
            // 闪电符号呼应“一键直达”的快捷操作语义。
            // A bolt mirrors the quick-actions "jump straight there" semantics.
            Self::QuickActions => "bolt.circle",
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
    /// Set a view frame without allowing AppKit's implicit layer action to race the explicit motion.
    /// 设置 view frame 时关闭 AppKit 隐式 layer 动画，避免与显式动效争抢控制权。
    unsafe fn set_frame_without_implicit_animation(view: *mut AnyObject, frame: NSRect) {
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![
            objc2::class!(CATransaction),
            setDisableActions: true
        ];
        let _: () = objc2::msg_send![view, setFrame: frame];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
    }

    /// Move a layer-backed view's center along the sidebar using one layer position animation.
    /// 使用单个 layer position 动画移动 layer-backed view 在侧栏中的中心位置。
    unsafe fn spring_move_view(
        view: *mut AnyObject,
        frame: NSRect,
        animation_key_name: &str,
        stiffness: f64,
        damping: f64,
    ) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            Self::set_frame_without_implicit_animation(view, frame);
            return;
        }
        // NSView backing layers are not guaranteed to use a centered anchor point. Derive the
        // target from the layer's actual anchor so `position` remains equivalent to this frame.
        // NSView backing layer 不保证使用中心锚点；根据实际 anchor 计算目标，让 position 与 frame 等价。
        let anchor: NSPoint = objc2::msg_send![layer, anchorPoint];
        let target_x = frame.origin.x + frame.size.width * anchor.x;
        let target_y = frame.origin.y + frame.size.height * anchor.y;
        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        let from_position: NSPoint = if presentation.is_null() {
            objc2::msg_send![layer, position]
        } else {
            objc2::msg_send![presentation, position]
        };
        let animation_key = crate::ffi::make_nsstring(animation_key_name);
        let _: () = objc2::msg_send![layer, removeAnimationForKey: animation_key];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![
            objc2::class!(CATransaction),
            setDisableActions: true
        ];
        let _: () = objc2::msg_send![
            layer,
            setPosition: NSPoint::new(target_x, target_y)
        ];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];

        let key_path = crate::ffi::make_nsstring("position.y");
        let from_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: from_position.y];
        let target_value: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithDouble: target_y];
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CASpringAnimation),
            animationWithKeyPath: key_path
        ];
        let _: () = objc2::msg_send![animation, setFromValue: from_value];
        let _: () = objc2::msg_send![animation, setToValue: target_value];
        let _: () = objc2::msg_send![animation, setMass: 0.6f64];
        let _: () = objc2::msg_send![animation, setStiffness: stiffness];
        let _: () = objc2::msg_send![animation, setDamping: damping];
        let _: () = objc2::msg_send![animation, setInitialVelocity: 0.0f64];
        let duration: f64 = objc2::msg_send![animation, settlingDuration];
        let _: () = objc2::msg_send![animation, setDuration: duration];
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        crate::ffi::CFRelease(animation_key as *const std::ffi::c_void);
    }

    /// Fade a sidebar background layer from its current presentation opacity to the target.
    /// 将侧栏背景图层从当前 presentation opacity 淡入或淡出到目标值。
    unsafe fn fade_view(view: *mut AnyObject, target_opacity: f32, animation_key_name: &str) {
        let layer: *mut AnyObject = objc2::msg_send![view, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![view, setAlphaValue: target_opacity as f64];
            return;
        }
        let presentation: *mut AnyObject = objc2::msg_send![layer, presentationLayer];
        let from_opacity: f32 = if presentation.is_null() {
            objc2::msg_send![layer, opacity]
        } else {
            objc2::msg_send![presentation, opacity]
        };
        let animation_key = crate::ffi::make_nsstring(animation_key_name);
        let _: () = objc2::msg_send![layer, removeAnimationForKey: animation_key];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![
            objc2::class!(CATransaction),
            setDisableActions: true
        ];
        let _: () = objc2::msg_send![layer, setOpacity: target_opacity];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        let key_path = crate::ffi::make_nsstring("opacity");
        let animation: *mut AnyObject = objc2::msg_send![
            objc2::class!(CABasicAnimation),
            animationWithKeyPath: key_path
        ];
        let from: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithFloat: from_opacity];
        let to: *mut AnyObject =
            objc2::msg_send![objc2::class!(NSNumber), numberWithFloat: target_opacity];
        let _: () = objc2::msg_send![animation, setFromValue: from];
        let _: () = objc2::msg_send![animation, setToValue: to];
        let _: () = objc2::msg_send![animation, setDuration: 0.15f64];
        let _: () = objc2::msg_send![layer, addAnimation: animation, forKey: animation_key];
        crate::ffi::CFRelease(key_path as *const std::ffi::c_void);
        crate::ffi::CFRelease(animation_key as *const std::ffi::c_void);
    }

    /// Move the active-row background with beUI's shared-layout spring.
    /// 使用 beUI 共享布局背景同款的 spring 移动选中行高亮。
    pub(super) unsafe fn move_highlight(highlight: *mut AnyObject, frame: NSRect, animated: bool) {
        if highlight.is_null() {
            return;
        }
        if !animated {
            Self::set_frame_without_implicit_animation(highlight, frame);
            return;
        }
        Self::spring_move_view(
            highlight,
            frame,
            "settings-sidebar-highlight-spring",
            360.0,
            32.0,
        );
    }

    /// Move the shared hover pill using the normal sidebar spring.
    /// 使用侧栏常规 spring 移动共享悬浮气泡。
    unsafe fn move_hover_highlight_with_spring(
        hover: *mut AnyObject,
        frame: NSRect,
        stiffness: f64,
        damping: f64,
    ) {
        if hover.is_null() {
            return;
        }
        let was_visible = widgets::SIDEBAR_HOVER_VISIBLE.swap(true, Ordering::SeqCst);
        if was_visible {
            Self::spring_move_view(
                hover,
                frame,
                "settings-sidebar-hover-spring",
                stiffness,
                damping,
            );
        } else {
            Self::set_frame_without_implicit_animation(hover, frame);
            Self::fade_view(hover, 1.0, "settings-sidebar-hover-opacity");
        }
    }

    /// Move the shared hover pill with the faster re-entry spring.
    /// 使用更快的重新进入 spring 移动共享悬浮气泡。
    pub(super) unsafe fn move_hover_highlight(hover: *mut AnyObject, frame: NSRect) {
        Self::move_hover_highlight_with_spring(hover, frame, 360.0, 32.0);
    }

    /// Prime the hover pill at the clicked row while keeping it invisible until the next row.
    /// 将悬停层预置到点击行并保持不可见，等待下一行进入时再播放移动动画。
    pub(super) unsafe fn prime_hover_highlight(hover: *mut AnyObject, frame: NSRect) {
        if hover.is_null() {
            return;
        }
        let layer: *mut AnyObject = objc2::msg_send![hover, layer];
        if layer.is_null() {
            Self::set_frame_without_implicit_animation(hover, frame);
            let _: () = objc2::msg_send![hover, setAlphaValue: 0.0f64];
            return;
        }
        let opacity_key = crate::ffi::make_nsstring("settings-sidebar-hover-opacity");
        let position_key = crate::ffi::make_nsstring("settings-sidebar-hover-spring");
        let _: () = objc2::msg_send![layer, removeAnimationForKey: opacity_key];
        let _: () = objc2::msg_send![layer, removeAnimationForKey: position_key];
        crate::ffi::CFRelease(opacity_key as *const std::ffi::c_void);
        crate::ffi::CFRelease(position_key as *const std::ffi::c_void);
        Self::set_frame_without_implicit_animation(hover, frame);
        let _: () = objc2::msg_send![layer, setOpacity: 0.0f32];
    }

    /// Move the primed hover pill from the clicked row and reveal it at the next row.
    /// 将预置在点击行的悬停层移动到下一行并同步淡入。
    pub(super) unsafe fn move_hover_highlight_after_selection(
        hover: *mut AnyObject,
        frame: NSRect,
    ) {
        if hover.is_null() {
            return;
        }
        Self::spring_move_view(hover, frame, "settings-sidebar-hover-spring", 360.0, 32.0);
        Self::fade_view(hover, 1.0, "settings-sidebar-hover-opacity");
    }

    pub(super) unsafe fn move_hover_highlight_on_reentry(hover: *mut AnyObject, frame: NSRect) {
        Self::move_hover_highlight_with_spring(hover, frame, 500.0, 30.0);
    }

    /// Hide the shared hover pill after the pointer leaves the whole menu.
    /// 指针离开整个菜单后隐藏共享悬浮气泡。
    pub(super) unsafe fn hide_hover_highlight(hover: *mut AnyObject) {
        if hover.is_null() {
            return;
        }
        if widgets::SIDEBAR_HOVER_VISIBLE.swap(false, Ordering::SeqCst) {
            Self::fade_view(hover, 0.0, "settings-sidebar-hover-opacity");
        }
    }

    /// Remove the hover surface immediately when a click promotes that row to selected.
    /// 点击将条目提升为选中态时立即移除悬浮层，避免与选中背景短暂重叠。
    pub(super) unsafe fn hide_hover_highlight_immediately(hover: *mut AnyObject) {
        if hover.is_null() {
            return;
        }
        widgets::SIDEBAR_HOVER_VISIBLE.store(false, Ordering::SeqCst);
        let layer: *mut AnyObject = objc2::msg_send![hover, layer];
        if layer.is_null() {
            let _: () = objc2::msg_send![hover, setAlphaValue: 0.0f64];
            return;
        }
        let opacity_key = crate::ffi::make_nsstring("settings-sidebar-hover-opacity");
        let position_key = crate::ffi::make_nsstring("settings-sidebar-hover-spring");
        let _: () = objc2::msg_send![layer, removeAnimationForKey: opacity_key];
        let _: () = objc2::msg_send![layer, removeAnimationForKey: position_key];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), begin];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), setDisableActions: true];
        let _: () = objc2::msg_send![layer, setOpacity: 0.0f32];
        let _: () = objc2::msg_send![objc2::class!(CATransaction), commit];
        crate::ffi::CFRelease(opacity_key as *const std::ffi::c_void);
        crate::ffi::CFRelease(position_key as *const std::ffi::c_void);
    }

    pub(super) unsafe fn build(
        parent: *mut AnyObject,
        target: *mut AnyObject,
        x: f64,
        y0: f64,
        w: f64,
    ) -> [*mut AnyObject; 7] {
        // Add new sidebar entries here: the component owns title keys, icons, tags, and spacing.
        // 新增侧栏入口只需在这里添加标题 key、图标和 tag；间距与对齐由组件统一处理。
        // 快捷操作位于窗口控制与关于之间。
        // Quick actions sits between Window control and About.
        let entries = [
            ("settings.sidebar_general", SettingsSidebarIcon::General),
            ("settings.sidebar_switcher", SettingsSidebarIcon::Switcher),
            ("settings.sidebar_mouse", SettingsSidebarIcon::Mouse),
            ("settings.sidebar_clipboard", SettingsSidebarIcon::Clipboard),
            (
                "settings.sidebar_window_control",
                SettingsSidebarIcon::WindowControl,
            ),
            (
                "settings.sidebar_quick_actions",
                SettingsSidebarIcon::QuickActions,
            ),
            ("settings.sidebar_about", SettingsSidebarIcon::About),
        ];
        widgets::make_sidebar_hover_highlight(parent, x, y0, w);
        widgets::make_sidebar_hover_tracking(parent, x, y0, w);
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
        assert_eq!(
            SettingsButtonRole::Destructive.style(),
            (0xFF3B30FF, 0xFFFFFFFF, -4)
        );
    }
}
