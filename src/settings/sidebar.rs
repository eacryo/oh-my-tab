//! Settings sidebar and two-pane background construction.

use super::*;

pub(super) struct SettingsSidebarGeometry {
    pub(super) content_h: f64,
    pub(super) view_w: f64,
    pub(super) card_margin: f64,
    pub(super) card_w: f64,
    pub(super) card_radius: f64,
}

pub(super) unsafe fn build_settings_sidebar(
    content: *mut AnyObject,
    geometry: SettingsSidebarGeometry,
    palette: UiPalette,
    target: *mut AnyObject,
    ui: &mut SettingsUi,
) {
    let SettingsSidebarGeometry {
        content_h,
        view_w,
        card_margin,
        card_w,
        card_radius,
    } = geometry;
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
    let app_title_font: *mut AnyObject = msg_send![class!(NSFont), boldSystemFontOfSize: 20.0f64];
    let _: () = msg_send![app_title, setFont: app_title_font];
    let app_title_color = settings_text_color(SettingsTextRole::Primary);
    let _: () = msg_send![app_title, setTextColor: app_title_color];
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
    let app_subtitle_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 12.0f64];
    let _: () = msg_send![app_subtitle, setFont: app_subtitle_font];
    let app_subtitle_color = settings_text_color(SettingsTextRole::Muted);
    let _: () = msg_send![app_subtitle, setTextColor: app_subtitle_color];
    let _: () = msg_send![app_subtitle, setAutoresizingMask: 12u64];
    let _: () = msg_send![sidebar_content, addSubview: app_subtitle];
    release_obj(app_subtitle);

    // Highlight background for the selected sidebar row (layer-backed NSView, theme-aware color);
    // added before the buttons so button titles draw on top of it.
    // Card-local layout: 12pt inner margins. The buttons stay close to the traffic lights;
    // btn_y0 is anchored to the full sidebar height rather than the toolbar-inset height.
    let btn_w = card_w - 28.0;
    let btn_h = SettingsSidebar::row_height(btn_w);
    // Sidebar navigation is also anchored to the full-height sidebar. Using layout_h here
    // includes the toolbar inset a second time and leaves a large blank gap above the title.
    let btn_y0 = content_h - card_margin - 112.0 - btn_h;
    let highlight: *mut AnyObject = msg_send![class!(NSView), alloc];
    let highlight: *mut AnyObject = msg_send![highlight, initWithFrame: NSRect::new(NSPoint::new(14.0, btn_y0), NSSize::new(btn_w, btn_h))];
    let _: () = msg_send![highlight, setAutoresizingMask: 12u64]; // top- and left-anchored
    let _: () = msg_send![highlight, setWantsLayer: true];
    let hl_layer: *mut AnyObject = msg_send![highlight, layer];
    let _: () = msg_send![hl_layer, setCornerRadius: 10.0f64];
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
        UPDATE_AVAILABLE.load(Ordering::SeqCst) || crate::restart::restart_required(),
    );

    // HTML `.sidebar-footer`: the complete restore control is one semantic component, with
    // its separator and morphing confirm/cancel rows owned together.
    ui.restore_defaults = RestoreDefaultsControl::build(sidebar_content, target, card_w);
}
