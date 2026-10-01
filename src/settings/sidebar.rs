//! Settings sidebar and two-pane background construction.

use super::*;

pub(super) struct SettingsSidebarGeometry {
    pub(super) content_h: f64,
    pub(super) view_w: f64,
    pub(super) card_margin: f64,
    pub(super) card_w: f64,
    pub(super) card_radius: f64,
}

pub(super) const SOLID_SIDEBAR_IDENTIFIER: &str = "settings-sidebar-solid-surface";
const SIDEBAR_APP_TITLE_BOTTOM_INSET: f64 = 78.0;
const SIDEBAR_NAV_GAP_AFTER_TITLE: f64 = 32.0;

pub(super) unsafe fn build_settings_sidebar(
    content: *mut AnyObject,
    geometry: SettingsSidebarGeometry,
    palette: UiPalette,
    target: *mut AnyObject,
    ui: &mut SettingsUi,
) {
    // The sidebar registries are keyed by raw view address, so building the sidebar invalidates
    // every address a previous build inserted: the old buttons are released with the old content
    // while their keys would survive. `sidebar_button_under_pointer` then messages those freed
    // pointers on the next hover, which traps in objc's receiver check. Clear here, where the
    // buttons are created, so every rebuild path -- including the in-place
    // `rebuild_settings_content` one, which never goes through window teardown -- drops them.
    widgets::clear_sidebar_view_registries();
    let SettingsSidebarGeometry {
        content_h,
        view_w,
        card_margin,
        card_w,
        card_radius,
    } = geometry;
    let card_h = content_h - card_margin * 2.0;
    let sidebar_view: *mut AnyObject = msg_send![class!(NSView), alloc];
    let sidebar_view: *mut AnyObject = msg_send![
        sidebar_view,
        initWithFrame: NSRect::new(
            NSPoint::new(card_margin, card_margin),
            NSSize::new(card_w, card_h)
        )
    ];
    let identifier = make_nsstring(SOLID_SIDEBAR_IDENTIFIER);
    let _: () = msg_send![sidebar_view, setIdentifier: identifier];
    release_obj(identifier);
    let _: () = msg_send![sidebar_view, setWantsLayer: true];
    // A plain layer keeps sidebar_bg as a solid color instead of compositing it through system glass.
    let sidebar_layer: *mut AnyObject = msg_send![sidebar_view, layer];
    if !sidebar_layer.is_null() {
        let _: () = msg_send![sidebar_layer, setCornerRadius: card_radius];
        let _: () = msg_send![sidebar_layer, setMasksToBounds: true];
        let _: () = msg_send![sidebar_layer, setOpaque: true];
        layer_set_background(
            sidebar_layer,
            crate::ffi::hex_to_cg_color(palette.sidebar_bg),
        );
    }
    let sidebar_content = sidebar_view;
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

    // The right detail pane has a flat token-colored surface (no radial highlight), directly
    // beside the gray sidebar.
    let main_background: *mut AnyObject =
        msg_send![widgets::settings_pane_surface_view_class(), alloc];
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
            NSPoint::new(24.0, content_h - SIDEBAR_APP_TITLE_BOTTOM_INSET),
            NSSize::new(card_w - 48.0, 26.0)
        )
    ];
    set_field(app_title, "Oh My Tab");
    let _: () = msg_send![app_title, setBezeled: false];
    let _: () = msg_send![app_title, setDrawsBackground: false];
    let _: () = msg_send![app_title, setEditable: false];
    let app_title_font: *mut AnyObject =
        msg_send![class!(NSFont), boldSystemFontOfSize: crate::theme::FONT_SIDEBAR_TITLE];
    let _: () = msg_send![app_title, setFont: app_title_font];
    let app_title_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![app_title, setTextColor: app_title_color];
    // Top- and left-anchored: the window height is adjustable, so the identity block must
    // follow the traffic-light strip instead of drifting downward.
    let _: () = msg_send![app_title, setAutoresizingMask: 12u64];
    let _: () = msg_send![sidebar_content, addSubview: app_title];
    release_obj(app_title);
    // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
    // added before the buttons so button titles draw on top of it.
    // Card-local layout: 12pt inner margins. The buttons stay close to the traffic lights;
    // btn_y0 is anchored to the full sidebar height rather than the toolbar-inset height.
    let btn_w = card_w - 24.0;
    let btn_h = SettingsSidebar::row_height(btn_w);
    // Sidebar navigation is also anchored to the full-height sidebar. Using layout_h here
    // includes the toolbar inset a second time and leaves a large blank gap above the title.
    let btn_y0 = content_h - SIDEBAR_APP_TITLE_BOTTOM_INSET - SIDEBAR_NAV_GAP_AFTER_TITLE - btn_h;
    let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
    let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(12.0, btn_y0), NSSize::new(btn_w, btn_h))];
    let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // top- and left-anchored
    let _: () = msg_send![highlight, setWantsLayer: true];
    let hl_layer: *mut AnyObject = msg_send![highlight, layer];
    let _: () = msg_send![hl_layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    // Selection highlight uses the system accent color (controlAccentColor), matching the
    // NSSwitch's on-state blue (same as LinearMouse's sidebar selection highlight).
    // The redesign uses a soft accent wash for the active row rather than a solid blue fill.
    layer_set_background(hl_layer, crate::ffi::hex_to_cg_color(palette.selection_bg));
    let _: () = msg_send![sidebar_content, addSubview: highlight];
    release_obj(highlight);
    ui.sidebar_highlight = highlight;

    // Sidebar buttons are created in page-index order; each tag selects the matching content view.
    let sidebar_buttons =
        SettingsSidebar::build(sidebar_content, target, 12.0, btn_y0, btn_w, btn_h);
    [
        &mut ui.sidebar_general,
        &mut ui.sidebar_switcher,
        &mut ui.sidebar_mouse,
        &mut ui.sidebar_clipboard,
        &mut ui.sidebar_window_control,
        &mut ui.sidebar_quick_actions,
        &mut ui.sidebar_keystroke_display,
        &mut ui.sidebar_about,
    ]
    .iter_mut()
    .zip(sidebar_buttons)
    .for_each(|(slot, button)| **slot = button);
    widgets::set_sidebar_update_indicator(
        ui.sidebar_about,
        UPDATE_AVAILABLE.load(Ordering::SeqCst) || crate::restart::restart_required(),
    );

    // HTML `.sidebar-footer`: the complete restore control is one semantic component, with
    // its separator and morphing confirm/cancel rows owned together.
    ui.restore_defaults = RestoreDefaultsControl::build(sidebar_content, target, card_w);
}
