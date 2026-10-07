//! The clipboard subsystem's smoke mode: the --smoke-clipboard end-to-end GUI run.
//! History file and image cache are redirected to a dedicated directory, never
//! touching real user data.

use super::*;

/// Smoke mode (--smoke-clipboard): the history file and the image cache are redirected to
/// a dedicated directory, never touching real user data (cfg!(test) is off in the real
/// binary, so this is the only isolation available).
pub(super) static SMOKE_MODE: AtomicBool = AtomicBool::new(false);

/// Enable smoke mode (called by main.rs on the --smoke-clipboard branch, before
/// smoke_runner).
pub(crate) fn set_smoke_mode() {
    SMOKE_MODE.store(true, Ordering::SeqCst);
}

/// --smoke-clipboard entry (called on the main thread): inject two entries, then show/hide
/// the picker twice to exercise rebuild_rows' row-cleanup path -- the site of a double-release
/// UAF that once segfaulted on the second summon. Returns true on success; a crash is a failure.
pub(crate) fn smoke_runner() -> bool {
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
        // A source-less entry: the header shows "unknown source", no icon.
        record_text(&mut hist, "legacy entry without a source", "", "", 50);
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
        // An image entry: written into the cache dir and referenced, covering the thumbnail
        // render/cleanup paths.
        let _ = cache_write_image(tiny_hash, TINY_PNG);
        let tiny = ImageEntry {
            uti: NSPASTEBOARD_TYPE_PNG.to_string(),
            hash: tiny_hash,
            data_path: clip_image_path(tiny_hash),
            preview_png: Arc::new(TINY_PNG.to_vec()),
            source_path: None,
        };
        record_image(&mut hist, &tiny, "Safari", "com.apple.Safari", 50);
        // Re-copying the same image: the content-hash dedup keeps the history from growing
        // (exercises image dedup).
        let before = hist.len();
        record_image(&mut hist, &tiny, "Safari", "com.apple.Safari", 50);
        assert_eq!(hist.len(), before, "same image must dedup");
    }
    show_picker();
    hide_picker();
    // Second show: rebuild_rows removes the old rows first (the former UAF path).
    show_picker();
    // The panel's outline is a decoration over the whole panel, so it has two ways to be wrong that no
    // token comparison would catch: absent (or present) against its own switch, and swallowing the
    // panel's input. Both are asserted here on the real view tree, including the `--panel-outline=off`
    // launch, where "no outline" must be what the switch produced rather than a silent install failure.
    unsafe {
        let backdrop = (*PICKER_BACKDROP.lock().unwrap())
            .expect("the picker's backdrop exists once the picker has been shown");
        let expected = crate::glass::panel_outline_enabled();
        match crate::glass::outline_state(&backdrop) {
            Some((width, colour_matches)) => {
                assert!(
                    expected,
                    "an outline was installed while the switch asked for none"
                );
                assert!(
                    (width - crate::theme::PANEL_OUTLINE_WIDTH).abs() < 1e-9,
                    "the outline's stroke width {width} is not the PANEL_OUTLINE_WIDTH token"
                );
                assert!(
                    colour_matches,
                    "the outline is not drawn in the card_border token"
                );
            }
            None => assert!(
                !expected,
                "the switch asked for an outline, but none was installed"
            ),
        }
        assert!(
            !crate::glass::outline_intercepts(&backdrop, crate::glass::outline_centre(&backdrop)),
            "the outline decoration must never be the hit-test result: it would swallow every click on the panel"
        );
        // The outline's colour must be the palette token, not whatever the layer happened to hold: the
        // theme's `card_border` differs per mode, so a stale value is a visibly wrong stroke. Only checked
        // when the outline is expected -- `--panel-outline=off` is a legitimate launch, and requiring a view
        // there would fail the very smoke that verifies the switch.
        if crate::glass::panel_outline_enabled() {
            let token = crate::theme::ui_palette().card_border;
            let expected = crate::ffi::hex_to_cg_color(token);
            let outline = crate::glass::outline_view_for_smoke(&backdrop)
                .expect("the outline view exists while the switch is on");
            let layer: *mut AnyObject = msg_send![outline, layer];
            assert!(
                !layer.is_null()
                    && crate::ffi::CGColorEqualToColor(
                        crate::ffi::layer_border_color(layer),
                        expected as *const c_void
                    ),
                "the outline must be drawn in the current theme's card_border"
            );
        }

        // The elevation shadow's silhouette has to enclose the *panel*: a path over the padded window would
        // put the shadow's darkest part in the padding, and a path never re-set after a resize would leave
        // the shadow the size of the previous panel. Both were wrong at least once while this was built, and
        // neither is visible in a token comparison, so both are asserted on the real geometry.
        //
        // Presence is asserted against the *effective* elevation rather than skipped when absent: a panel
        // that should have a shadow and has no path is exactly the silent-pass this check exists to stop.
        let shadow = crate::glass::carrier_shadow_path_rect(&backdrop);
        let expects_shadow = crate::glass::effective_panel_elevation_id() != "none";
        assert_eq!(
            shadow.is_some(),
            expects_shadow,
            "the picker's elevation is {} but its shadow path {} installed",
            crate::glass::effective_panel_elevation_id(),
            if shadow.is_some() { "is" } else { "is not" }
        );
        if let Some(shadow) = shadow {
            let window = (*PICKER_WINDOW.lock().unwrap())
                .expect("the picker window exists once the picker has been shown")
                .0;
            let panel = crate::glass::panel_frame_of(window);
            let insets = crate::glass::panel_insets_of(window);
            let padded = crate::theme::window_frame_for_panel(panel, insets);
            assert!(
                (shadow.w - panel.size.width).abs() < 1.0
                    && (shadow.h - panel.size.height).abs() < 1.0,
                "the shadow path must enclose the panel ({}x{}), not the padded window ({}x{})",
                panel.size.width,
                panel.size.height,
                padded.size.width,
                padded.size.height
            );
            // The panel sits inside the window by the padding, so the path must be offset by it: a path at
            // the window's origin would be the "expanded twice" mistake.
            assert!(
                (shadow.x - insets.left).abs() < 1.0 && (shadow.y - insets.bottom).abs() < 1.0,
                "the shadow path must sit at the panel's offset inside the window ({}, {}), not at {}",
                insets.left,
                insets.bottom,
                shadow.x
            );
        }
    }
    assert!(
        unsafe { detail::smoke_row_backdrop_paint_paths() },
        "selected+hovered row backdrops must match in the build and runtime repaint paths"
    );
    // An active detail icon is a filled chip, so its glyph has to be readable *against its own
    // fill*. Measured on the pixels rather than on the tokens, because the failure was a wrong
    // role: the fill was `primary_text` while the glyph stayed `accent_text`, which is 13.91:1 in
    // light mode but 1.09:1 in dark mode, where the icon became a blank white disc.
    unsafe {
        assert!(
            active_detail_icon_glyph_is_legible_on_its_fill(),
            "an active detail icon must draw its glyph on its own fill"
        );
    }

    /// The two clear actions must show (and reserve room for) the scope the current filter and query
    /// describe, in the locale in effect. Runs on the main thread: the state lives behind
    /// `MainThreadSlot`s. Every path that can change the scope or the locale has to leave this true.
    unsafe fn assert_clear_actions_match_scope() {
        let applied =
            || super::text_style::clear_action_applied().expect("the clear actions applied");
        let (unpinned, all, widths) = applied();
        let expected = super::clip_clear_labels(
            *super::CLIP_FILTER.lock().unwrap(),
            super::with_clipboard_ui(|ui| !ui.search_query.is_empty()),
        );
        assert_eq!(
            (unpinned, all),
            expected,
            "the clear actions must name the scope they clear"
        );
        let titles: Vec<String> = {
            let buttons = (*CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap())
                .expect("persistent clear action buttons must be built");
            buttons
                .iter()
                .map(|button| {
                    let title: *mut AnyObject = msg_send![button.0, title];
                    crate::ffi::nsstring_to_rust(title)
                })
                .collect()
        };
        assert_eq!(
            titles,
            vec![expected.0.clone(), expected.1.clone()],
            "the buttons must show the scope they clear"
        );
        let widest: [f64; 2] = std::array::from_fn(|index| {
            super::clip_clear_label_variants()[index]
                .iter()
                .map(|label| super::localized_string_width(label, crate::theme::FONT_CAPTION) + 8.0)
                .fold(0.0_f64, f64::max)
        });
        assert!(
            widths[0] >= widest[0] && widths[1] >= widest[1],
            "reserved {widths:?} must cover the widest variant {widest:?}"
        );
    }

    // Footer shortcut legends must survive the fonts they are drawn with, in every shipped
    // locale: a label whose frame was sized for a smaller font than the one it renders with
    // wraps out of its one-line-high field and silently loses its tail.
    unsafe {
        assert!(
            footer_legends_layout_is_sane(),
            "clipboard footer legends must render fully"
        );
        let original_locale = CONFIG.read().unwrap().i18n.locale.clone();
        // A query is active so the scope wording (the widest variant) is what must survive.
        super::with_clipboard_ui(|ui| ui.search_query = "apple".to_string());
        update_clear_action_labels();
        for locale in ["en", "zh-Hans", "zh-Hant"] {
            crate::i18n::apply_config_locale(locale);
            refresh_localized_ui();
            assert!(
                footer_legends_layout_is_sane(),
                "clipboard footer legends must render fully in locale={locale}"
            );
            // A locale refresh re-measures the reservation and re-applies the current scope; it
            // used to reset both to the two short labels.
            assert_clear_actions_match_scope();
        }
        super::with_clipboard_ui(|ui| ui.search_query.clear());
        crate::i18n::apply_config_locale(&original_locale);
        refresh_localized_ui();
    }
    // Delete/undo GUI smoke: delete the top image through the real list-focus path, then use
    // Cmd+Z to restore it; its cache must survive the undo window without duplication.
    unsafe {
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            set_picker_selection(0);
            rebuild_rows();
            let original = {
                let hist = CLIP_HISTORY.lock().unwrap();
                hist.iter().find(|entry| entry.image.is_some()).cloned()
            }
            .expect("clipboard smoke must contain an image entry");
            let hash = original.image.as_ref().unwrap().hash;
            let ev_delete = make_key_event(51);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_delete as *mut c_void);
            assert!(
                !CLIP_HISTORY
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|entry| same_clip_entry_identity(entry, &original)),
                "backspace must remove the selected clipboard entry"
            );
            let ev_undo = make_key_event_with_modifiers(6, 0x0010_0000);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_undo as *mut c_void);
            let hist = CLIP_HISTORY.lock().unwrap();
            assert_eq!(
                hist.iter()
                    .filter(|entry| same_clip_entry_identity(entry, &original))
                    .count(),
                1,
                "Cmd+Z must restore exactly one entry"
            );
            assert!(
                cache_read_image(hash).is_some(),
                "undo must retain image bytes"
            );
        }
    }
    // Clear-entry GUI smoke: both clear actions remain visible side by side without an
    // expansion card; clear scopes are covered by pure logic tests so this smoke retains all
    // fixtures for the subsequent detail path.
    unsafe {
        let buttons = (*CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap())
            .expect("persistent clear action buttons must be built");
        let frames: [NSRect; 2] = buttons.map(|button| msg_send![button.0, frame]);
        assert!(frames[0].origin.x < frames[1].origin.x);
        assert_eq!(frames[0].origin.y, frames[1].origin.y);
        assert_eq!(
            frames[1].origin.x - (frames[0].origin.x + frames[0].size.width),
            super::CLEAR_ACTION_GAP
        );
        let header: *mut AnyObject = msg_send![buttons[0].0, superview];
        let parent: *mut AnyObject = msg_send![header, superview];
        for button in buttons {
            let bounds: NSRect = msg_send![button.0, bounds];
            let in_parent: NSRect = msg_send![button.0, convertRect: bounds, toView: parent];
            let center = NSPoint::new(
                in_parent.origin.x + in_parent.size.width / 2.0,
                in_parent.origin.y + in_parent.size.height / 2.0,
            );
            let hit: *mut AnyObject = msg_send![parent, hitTest: center];
            assert_eq!(
                hit, button.0,
                "clear action button must remain directly hit-testable"
            );
        }
        let pills: Vec<*mut AnyObject> = FILTER_PILLS
            .lock()
            .unwrap()
            .iter()
            .map(|pill| pill.0)
            .collect();
        assert!(
            pills.iter().all(|pill| {
                let hidden: bool = msg_send![*pill, isHidden];
                !hidden
            }),
            "filter tabs must remain visible beside clear actions"
        );
    }
    // Clear-action scope smoke: a typed query names the result scope, clearing the query through the
    // shared path (the × key, the field's Esc, a fresh summon) restores the category wording, and the
    // reservation always covers the widest variant. Clearing the query by hand in the list-focus Esc
    // branch used to leave "results" on the buttons.
    unsafe {
        super::with_clipboard_ui(|ui| ui.search_query = "apple".to_string());
        update_clear_action_labels();
        assert_eq!(
            super::text_style::clear_action_applied().unwrap().0,
            super::t("clipboard.clear_scope_unpinned_results"),
            "a typed query must name the result scope"
        );
        clear_search();
        assert_clear_actions_match_scope();
    }
    // Search smoke: set a query -> rebuild (filtered display) -> arrow navigation within the
    // filtered list -> clear restores everything.
    unsafe {
        super::with_clipboard_ui(|ui| ui.search_query = "apple".to_string());
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(125); // ↓ / down arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
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
        // Up at the list top: focus jumps back to the search field (query kept); the
        // search field's begin-editing delegate callback (search_field_began_editing) clears
        // the selection -> the highlight disappears.
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            let ev = make_key_event(126); // ↑ / up arrow
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
        }
        // Assert: after focus enters the search field the selection is cleared (no row
        // highlight).
        assert_eq!(
            super::picker_selection(),
            NO_SELECTION,
            "selection must clear when focus moves into the search field"
        );
        // Esc level one: clear the query and restore the full list (list-focus path).
        let ev_esc = make_key_event(53);
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev_esc as *mut c_void);
        }
        clear_search();
    }
    // Keyboard-navigation smoke: build a real NSEvent and drive container_key_down, covering
    // arrow -> select -> scroll-into-view (once panicked on a wrong return-type encoding for
    // scrollRectToVisible:).
    unsafe {
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
            // Scroll several times: each scroll fires the clip-view bounds-change notification
            // -> the indicator-update path (setOpacity: once panicked on a float/double
            // encoding mismatch; the smoke run must cover it).
            for step in 1..=5u8 {
                let _: () = msg_send![c.0, scrollPoint: NSPoint::new(0.0, step as f64 * 30.0)];
            }
        }
    }
    // Detail/pin smoke: → opens the detail (text), ↓ follows it (stays open), ← pins while
    // keeping the detail open, and → closes it; an image entry's → opens its detail (lazy
    // .detail generation); with the detail open, Esc's level one closes the detail only.
    unsafe {
        super::set_picker_selection(0);
        rebuild_rows();
        let c_opt = *PICKER_CONTAINER.lock().unwrap();
        if let Some(c) = c_opt {
            // Right: expand the detail for display 0 (the newest recorded entry, an image ->
            // the image branch).
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                super::detail_visible(),
                "right arrow must open the detail panel"
            );
            // Down: the detail follows the selection (stays open); the selection moves to a
            // text entry -> the full-text branch.
            let ev = make_key_event(125);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                super::detail_visible(),
                "the detail must stay open while navigating"
            );
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
            // No-wrap mode carries the source map too (midpoints depend on it for lossless copy), and the
            // displayed text must contain midpoint markers and no U+2028 other than real spaces.
            assert!(DETAIL_SOURCE_MAP.lock().unwrap().is_some());
            assert!(
                no_wrap_frame.size.width > no_wrap_bounds.size.width,
                "long source lines must overflow the horizontal viewport"
            );

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
            // 0 = NSScrollElasticityAutomatic: endpoint rubber banding is native; the
            // bounds notification must never rewrite the clip view (hard-clamping fights
            // momentum and twitches the scrollbar).
            assert_eq!(
                elasticity, 0,
                "detail must keep native automatic rubber-band elasticity"
            );
            assert!(!noncontiguous, "detail layout must be contiguous");
            assert!(!background, "detail background layout must be disabled");
            // The bounds callback may update only the capsule, never the clip view. Some
            // macOS versions synchronously clamp setBoundsOrigin, so compare the actual
            // origin before and after the callback instead of assuming overscroll can always
            // be constructed.
            let _: () = msg_send![
                detail_clip,
                setBoundsOrigin: NSPoint::new(opened_bounds.origin.x, min_y - 30.0)
            ];
            let top_before_callback: NSRect = msg_send![detail_clip, bounds];
            detail_scroll_indicator_bounds_changed(
                observer() as *mut c_void,
                sel!(detailScrollIndicatorBoundsChanged:),
                std::ptr::null_mut(),
            );
            let top_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                top_bounds.origin.y, top_before_callback.origin.y,
                "top bounds callback must not rewrite the clip view"
            );
            let _: () = msg_send![
                detail_clip,
                setBoundsOrigin: NSPoint::new(opened_bounds.origin.x, max_y + 30.0)
            ];
            let bottom_before_callback: NSRect = msg_send![detail_clip, bounds];
            detail_scroll_indicator_bounds_changed(
                observer() as *mut c_void,
                sel!(detailScrollIndicatorBoundsChanged:),
                std::ptr::null_mut(),
            );
            let bottom_bounds: NSRect = msg_send![detail_clip, bounds];
            assert_eq!(
                bottom_bounds.origin.y, bottom_before_callback.origin.y,
                "bottom bounds callback must not rewrite the clip view"
            );
            scroll_detail_to_top(detail_scroll);
            // Left: pin the selected entry while the detail stays open and follows its new row.
            let pinned_text = {
                let sel_idx = super::picker_selection();
                let hist = CLIP_HISTORY.lock().unwrap();
                mapped_index(sel_idx)
                    .and_then(|h| hist.get(h))
                    .map(|e| e.text.clone())
            };
            let ev = make_key_event(123);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                super::detail_visible(),
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
            // Right with the detail open: close the detail panel.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !super::detail_visible(),
                "right arrow must close the detail when it is open"
            );
            // Reopen with →, then close with → again to cover the detail toggle path.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(super::detail_visible());
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(
                !super::detail_visible(),
                "right arrow must close the detail when it is open"
            );
            // Esc with the detail open: level one closes the detail; the picker stays.
            let ev = make_key_event(124);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(super::detail_visible());
            let ev = make_key_event(53);
            container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
            assert!(!super::detail_visible(), "Esc must close the detail first");
        }
        // Image entry: locate its display index, → expands it -> the lazy .detail preview is
        // generated and cached.
        let img_h = {
            let hist = CLIP_HISTORY.lock().unwrap();
            hist.iter().position(|e| e.image.is_some())
        };
        if let Some(h_idx) = img_h {
            let d_idx = super::with_clipboard_ui(|ui| ui.filtered.iter().position(|&h| h == h_idx));
            if let Some(d_idx) = d_idx {
                super::set_picker_selection(d_idx);
                rebuild_rows();
                let c_opt = *PICKER_CONTAINER.lock().unwrap();
                if let Some(c) = c_opt {
                    let ev = make_key_event(124);
                    container_key_down(c.0 as *mut c_void, sel!(keyDown:), ev as *mut c_void);
                    assert!(super::detail_visible(), "image detail must open");
                    // The hi-res detail image is generated on a worker; allow a short window
                    // for disk delivery so asynchronous work is not mistaken for a UI failure.
                    for _ in 0..20 {
                        if cache_read_detail_preview(tiny_hash).is_some() {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    assert!(
                        cache_read_detail_preview(tiny_hash).is_some(),
                        "the lazy .detail preview must be generated on first open"
                    );
                }
            }
        }
    }
    // Empty-state placement: the hint's vertical center and the document height are derived from
    // the live viewport, but the row cache key carries no geometry and the clear actions rebuild in
    // place without resizing the panel. Reproduces the reported "empty picker with no hint and a
    // phantom scrollbar": drop the fixtures while the panel is still tall, then summon (which
    // shrinks it to the minimal height) and require the hint to be re-centered inside the viewport.
    unsafe {
        {
            let mut hist = CLIP_HISTORY.lock().unwrap();
            remove_history_scope(&mut hist, true, ClipFilter::All, "");
        }
        clear_search();
        *CLIP_FILTER.lock().unwrap() = ClipFilter::All;
        rebuild_rows();
        hide_picker();
        show_picker();
        assert!(
            empty_state_layout_is_sane(),
            "the empty-state hint must be re-centered inside the summoned viewport"
        );
    }
    // A hi-res preview that finished generating before a discard must not be delivered: the slot
    // carries the generation it was generated in, and the main-thread consumer refuses a stale one
    // (the worker stores through the same check, so neither end of the pipe can resurrect a deleted
    // entry's preview).
    {
        // Both axes stale: a discard is only one of the ways a preview loses its right to be shown.
        let stale = super::cache_generation().wrapping_sub(1);
        *DETAIL_PENDING_HD.lock().unwrap() = Some((12345, stale, 0, vec![1, 2, 3]));
        detail_preview_ready(
            std::ptr::null_mut(),
            sel!(detailPreviewReady:),
            std::ptr::null_mut(),
        );
        assert!(
            DETAIL_PENDING_HD.lock().unwrap().is_none(),
            "a preview generated before a discard must be refused and released"
        );
    }
    // Switch-off versus queued image work (the reviewer's scenario: copy an image, turn the feature
    // off immediately). Recording hands the original bytes to a background cache job; discarding the
    // history must invalidate those jobs and wait out a write in flight, or the file comes back
    // after the wipe. The wait below gives the worker time to run a stale job if the generation
    // guard is missing.
    {
        // Its own payload, so the fixture image above cannot mask the result.
        let bytes = std::sync::Arc::new(b"switch-off-during-queue".to_vec());
        let hash = crate::hash::fnv1a64(&bytes);
        super::schedule_image_cache_write(
            hash,
            Some(bytes.clone()),
            std::sync::Arc::new(bytes.as_ref().clone()),
            None,
            true,
        );
        super::clear_history_state();
        assert!(
            !super::clip_image_path(hash).exists(),
            "discarding the history must not leave the image cache behind"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !super::clip_image_path(hash).exists(),
            "a job queued before the discard must not recreate the cache file"
        );
        // Files only: in a smoke run the harness keeps the history file in a subdirectory of the
        // cache directory, and the wipe removes files, not directories.
        let leftovers: Vec<String> = std::fs::read_dir(super::clip_image_cache_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().is_file())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            leftovers.is_empty(),
            "the image cache must be empty after a discard, found {leftovers:?}"
        );
    }
    hide_picker();
    true
}

/// Build an arrow-key NSEvent (for the smoke run).
unsafe fn make_key_event(keycode: u16) -> *mut AnyObject {
    make_key_event_with_modifiers(keycode, 0)
}

/// Build an NSEvent with modifier flags for smoke tests.
unsafe fn make_key_event_with_modifiers(keycode: u16, modifiers: u64) -> *mut AnyObject {
    let chars = make_nsstring("x");
    // keyEventWithType: takes NSEventType (unsigned long), location, modifierFlags, timestamp,
    // windowNumber (NSInteger), context, characters, charactersIgnoringModifiers, isARepeat,
    // keyCode (unsigned short).
    let ev: *mut AnyObject = msg_send![
        class!(NSEvent),
        keyEventWithType: 10u64,
        location: NSPoint::new(0.0, 0.0),
        modifierFlags: modifiers,
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

/// Rasterise an active detail icon and verify its two halves: the chip is filled with the accent
/// and its glyph is drawn in `accent_text` over it. Reading the pixels catches a mismatched role
/// (a fill the glyph cannot be seen against), which a token-level check cannot express.
unsafe fn active_detail_icon_glyph_is_legible_on_its_fill() -> bool {
    // Both appearances, not just the one this process resolves to: the defect was dark-only.
    let mut ok = true;
    for dark in [false, true] {
        if !active_detail_icon_glyph_is_legible_for_mode(dark) {
            ok = false;
        }
    }
    ok
}

/// Render an active detail icon and require its glyph to be legible *against its own fill*.
///
/// Measured from the pixels because the defect was a wrong role: the chip was filled with
/// `primary_text` while the glyph stayed `accent_text`. In light mode that is a dark chip with a
/// white glyph (13.91:1, fine), but dark `primary_text` is near-white, so the glyph measured
/// 1.09:1 and the icon showed as a blank disc. A token-level check cannot express "these two
/// roles are readable together"; the dominant colour is the fill, and the glyph is whatever
/// other colour the drawing put on top of it.
unsafe fn active_detail_icon_glyph_is_legible_for_mode(dark: bool) -> bool {
    // The floor for a non-text indicator (design-style §3.3).
    const MIN_GLYPH_CONTRAST: f64 = 3.0;
    let image = super::text_style::make_detail_action_icon_for_mode(dark, true, false);
    if image.is_null() {
        return false;
    }
    let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
    if tiff.is_null() {
        release_obj(image);
        return false;
    }
    let rep: *mut AnyObject = msg_send![class!(NSBitmapImageRep), alloc];
    let rep: *mut AnyObject = msg_send![rep, initWithData: tiff];
    if rep.is_null() {
        release_obj(image);
        return false;
    }
    let width: usize = msg_send![rep, pixelsWide];
    let height: usize = msg_send![rep, pixelsHigh];
    let mut histogram: Vec<([f64; 3], usize)> = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let color: *mut AnyObject = msg_send![rep, colorAtX: x, y: y];
            if color.is_null() {
                continue;
            }
            let alpha: f64 = msg_send![color, alphaComponent];
            if alpha < 0.9 {
                continue;
            }
            let rgb: [f64; 3] = [
                msg_send![color, redComponent],
                msg_send![color, greenComponent],
                msg_send![color, blueComponent],
            ];
            let mut bucket: Option<usize> = None;
            for (index, (existing, _)) in histogram.iter().enumerate() {
                if (0..3usize).all(|channel| (existing[channel] - rgb[channel]).abs() < 0.04) {
                    bucket = Some(index);
                    break;
                }
            }
            match bucket {
                Some(index) => histogram[index].1 += 1,
                None => histogram.push((rgb, 1)),
            }
        }
    }
    release_obj(rep);
    release_obj(image);
    let Some((fill, fill_count)) = histogram.iter().max_by_key(|(_, count)| *count).cloned() else {
        return false;
    };
    if fill_count < 20 {
        eprintln!(
            "[smoke-clipboard] active detail icon has no filled chip in {} mode",
            if dark { "dark" } else { "light" }
        );
        return false;
    }
    // The glyph is the most prominent *other* colour: a solid disc with no visible glyph fails.
    let glyph = histogram
        .iter()
        .filter(|(rgb, count)| *count >= 4 && (0..3usize).any(|c| (rgb[c] - fill[c]).abs() > 0.04))
        .max_by(|(a, _), (b, _)| {
            let ca = crate::theme::contrast_ratio_srgb(*a, fill);
            let cb = crate::theme::contrast_ratio_srgb(*b, fill);
            ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned();
    let Some((glyph, _)) = glyph else {
        eprintln!(
            "[smoke-clipboard] active detail icon is a blank disc in {} mode",
            if dark { "dark" } else { "light" }
        );
        return false;
    };
    let contrast = crate::theme::contrast_ratio_srgb(glyph, fill);
    if contrast < MIN_GLYPH_CONTRAST {
        eprintln!(
            "[smoke-clipboard] active detail icon glyph measures {contrast:.2}:1 on its own fill in {} mode (min {MIN_GLYPH_CONTRAST})",
            if dark { "dark" } else { "light" }
        );
        return false;
    }
    true
}
