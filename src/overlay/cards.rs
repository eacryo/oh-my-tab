//! Card-view construction/rendering (icon & thumbnail, grayed icons) and show_overlay layout.

use super::*;

/// Bake a grayed version: composite a light gray over the original with NSCompositeSourceAtop,
/// so the gray is confined to the icon's alpha and doesn't form a box on transparent edges.
/// Used to gray out minimized windows' icons.
unsafe fn grayed_image(orig: *mut AnyObject, size: NSSize) -> *mut AnyObject {
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithSize: size];
    let _: () = msg_send![img, lockFocus];
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    // Draw the original image first (NSCompositeSourceOver = 2).
    let zero_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let _: () =
        msg_send![orig, drawInRect: rect, fromRect: zero_rect, operation: 2isize, fraction: 1.0f64];
    // Then composite light grey with SourceAtop (= 5): tint only where alpha already exists, never
    // outside the icon.
    let ctx: *mut AnyObject = msg_send![class!(NSGraphicsContext), currentContext];
    let _: () = msg_send![ctx, setCompositingOperation: 5isize];
    let gray = hex_to_ns_color(0x808080AA);
    let _: () = msg_send![gray, setFill];
    let _: () = msg_send![class!(NSBezierPath), fillRect: rect];
    let _: () = msg_send![ctx, setCompositingOperation: 2isize]; // restore SourceOver
    let _: () = msg_send![img, unlockFocus];
    img
}

/// CGImageRef -> NSImage at a given point size. CG/CF types cannot go through
/// objc2's msg_send! -- a bare c_void encodes as '^v' while the method expects
/// '^{CGImage=}', and the runtime panics (verified: it blew up show_overlay while
/// it held the TAB_STATE lock, and the poisoned lock then crashed every following
/// Cmd+Tab). Follows the layer_set_background raw objc_msgSend convention.
pub(crate) unsafe fn nsimage_from_cgimage(cg: *const c_void, size: NSSize) -> *mut AnyObject {
    let sel = sel!(initWithCGImage:size:);
    type F = unsafe extern "C" fn(*mut AnyObject, Sel, *const c_void, NSSize) -> *mut AnyObject;
    let f: F = std::mem::transmute(objc_msgSend as *const ());
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    if img.is_null() {
        return std::ptr::null_mut();
    }
    f(img, sel, cg, size)
}

/// A left-aligned label (the mockup's .caption-title): fixed width + tail
/// truncation, no centering.
unsafe fn make_left_label(
    text: &str,
    font: *mut AnyObject,
    color: *mut AnyObject,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> *mut AnyObject {
    let ns_str = make_nsstring(text);
    let init_frame = NSRect::new(NSPoint::new(x, y), NSSize::new(width, height));
    let label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let label: *mut AnyObject = msg_send![label, initWithFrame: init_frame];
    let _: () = msg_send![label, setStringValue: ns_str];
    CFRelease(ns_str as *const c_void);
    let _: () = msg_send![label, setBezeled: false];
    let _: () = msg_send![label, setDrawsBackground: false];
    let _: () = msg_send![label, setEditable: false];
    let _: () = msg_send![label, setSelectable: false];
    let _: () = msg_send![label, setUsesSingleLineMode: true];
    let _: () = msg_send![label, setAlignment: 0isize]; // NSTextAlignmentLeft
    let _: () = msg_send![label, setFont: font];
    let _: () = msg_send![label, setTextColor: color];
    // Tail truncation (NSLineBreakByTruncatingTail = 4): ellipsis on overflow.
    let _: () = msg_send![label, setLineBreakMode: 4isize];
    let ascender: f64 = msg_send![font, ascender];
    let descender: f64 = msg_send![font, descender];
    let line_h = (ascender - descender + 1.0).max(11.0).min(height.max(1.0));
    let centered_y = y + (height - line_h) / 2.0;
    let _: () = msg_send![label, setFrame: NSRect::new(
        NSPoint::new(x, centered_y),
        NSSize::new(width, line_h),
    )];
    label
}

/// Fallback content for a preview without a thumbnail: a centered large app icon
/// (or first-letter block), grayscale-baked when minimized. Laid out in the
/// container's local coordinates (container is pw×ph).
unsafe fn add_preview_icon_fallback(
    container: *mut AnyObject,
    pw: f64,
    ph: f64,
    w: &WindowInfo,
    colors: &Colors,
) {
    let big = (ph - 16.0).clamp(36.0, 72.0);
    let cx = (pw - big) / 2.0;
    let cy = (ph - big) / 2.0;
    let frame = NSRect::new(NSPoint::new(cx, cy), NSSize::new(big, big));
    let mut loaded: Option<*mut AnyObject> = None;
    if let Some(ref icon_path) = w.icon_path {
        let ns_path = make_nsstring(icon_path);
        let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
        let img: *mut AnyObject = msg_send![img, initWithContentsOfFile: ns_path];
        CFRelease(ns_path as *const c_void);
        if !img.is_null() {
            loaded = Some(img);
        }
    }
    if let Some(img) = loaded {
        let shown: *mut AnyObject = if w.minimized {
            let g = grayed_image(img, NSSize::new(big, big));
            release_obj(img);
            g
        } else {
            img
        };
        let iv: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let iv: *mut AnyObject = msg_send![iv, initWithFrame: frame];
        let _: () = msg_send![iv, setImage: shown];
        release_obj(shown);
        let _: () = msg_send![iv, setImageScaling: 3u64];
        let _: () = msg_send![container, addSubview: iv];
        release_obj(iv);
    } else {
        // First-letter block (rounded + icon_inner_bg + initial), same style as the
        // legacy letter placeholder.
        let lv: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let lv: *mut AnyObject = msg_send![lv, initWithFrame: frame];
        let _: () = msg_send![lv, setWantsLayer: true];
        let ll: *mut AnyObject = msg_send![lv, layer];
        let _: () = msg_send![ll, setCornerRadius: 12.0f64];
        let _: () = msg_send![ll, setMasksToBounds: true];
        layer_set_background(ll, hex_to_cg_color(colors.icon_inner_bg));
        let init_char = w.app_name.chars().next().unwrap_or('?').to_string();
        let font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: 24.0f64, weight: 0.4f64];
        let label = make_centered_label(
            &init_char,
            font,
            hex_to_ns_color(colors.icon_text),
            0.0,
            big,
            big,
        );
        let _: () = msg_send![lv, addSubview: label];
        release_obj(label);
        if w.minimized {
            let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
            let dim: *mut AnyObject = msg_send![dim, initWithFrame: frame];
            let _: () = msg_send![dim, setWantsLayer: true];
            let dl: *mut AnyObject = msg_send![dim, layer];
            let _: () = msg_send![dl, setCornerRadius: 12.0f64];
            let _: () = msg_send![dl, setMasksToBounds: true];
            layer_set_background(dl, hex_to_cg_color(0x808080AA));
            let _: () = msg_send![lv, addSubview: dim];
            release_obj(dim);
        }
        let _: () = msg_send![container, addSubview: lv];
        release_obj(lv);
    }
}

fn needs_visibility_badge(minimized: bool, app_hidden: bool) -> bool {
    minimized || app_hidden
}

unsafe fn add_visibility_badge_if_needed(
    container: *mut AnyObject,
    preview_width: f64,
    preview_height: f64,
    w: &WindowInfo,
) {
    if !needs_visibility_badge(w.minimized, w.app_hidden) {
        return;
    }

    let symbol_size = 22.0f64.min(preview_width).min(preview_height);
    let symbol_frame = NSRect::new(
        NSPoint::new(
            (preview_width - THUMB_PAD - symbol_size).max(0.0),
            (preview_height - THUMB_PAD - symbol_size).max(0.0),
        ),
        NSSize::new(symbol_size, symbol_size),
    );
    let symbol_name = make_nsstring("eye.slash.fill");
    let symbol: *mut AnyObject = msg_send![
        class!(NSImage),
        imageWithSystemSymbolName: symbol_name,
        accessibilityDescription: std::ptr::null::<AnyObject>()
    ];
    CFRelease(symbol_name as *const c_void);
    if !symbol.is_null() {
        let _: () = msg_send![symbol, setTemplate: true];
        let _: () = msg_send![symbol, setSize: NSSize::new(symbol_size, symbol_size)];
        let icon: *mut AnyObject = msg_send![class!(NSImageView), alloc];
        let icon: *mut AnyObject = msg_send![icon, initWithFrame: symbol_frame];
        let _: () = msg_send![icon, setWantsLayer: true];
        let _: () = msg_send![icon, setImage: symbol];
        let _: () = msg_send![icon, setImageScaling: 3u64];
        let _: () = msg_send![icon, setContentTintColor: hex_to_ns_color(0xFFFFFFFF)];
        let icon_layer: *mut AnyObject = msg_send![icon, layer];
        layer_set_shadow_color(icon_layer, hex_to_cg_color(0x000000B3));
        let _: () = msg_send![icon_layer, setShadowOpacity: 0.85f32];
        let _: () = msg_send![icon_layer, setShadowRadius: 2.0f64];
        let _: () = msg_send![icon_layer, setShadowOffset: NSSize::new(0.0, -1.0)];
        let _: () = msg_send![container, addSubview: icon];
        release_obj(icon);
    }
}

/// Replace only the image/icon content inside a preview container, preserving the
/// card caption, buttons, tracking area, layers, and selection state. Asynchronous
/// thumbnail delivery no longer destroys and rebuilds the whole card.
pub(super) unsafe fn populate_thumbnail_preview(
    container: *mut AnyObject,
    w: &WindowInfo,
    colors: &Colors,
    capture_allowed: bool,
) {
    let old: *mut AnyObject = msg_send![container, subviews];
    let mut old_count: usize = msg_send![old, count];
    while old_count > 0 {
        old_count -= 1;
        let child: *mut AnyObject = msg_send![old, objectAtIndex: old_count];
        let _: () = msg_send![child, removeFromSuperview];
    }

    let bounds: NSRect = msg_send![container, bounds];
    let pw = bounds.size.width;
    let ph = bounds.size.height;
    let thumb = if capture_allowed && w.bounds.2 > 0.0 && w.bounds.3 > 0.0 {
        crate::thumbnail::lookup_retained(w.pid, w.window_id)
    } else {
        None
    };
    let Some((cg, w_px, h_px)) = thumb else {
        add_preview_icon_fallback(container, pw, ph, w, colors);
        add_visibility_badge_if_needed(container, pw, ph, w);
        return;
    };

    let (cw, ch) = crate::thumbnail::fit_size(w_px as f64, h_px as f64, pw, ph);
    let nsimg = nsimage_from_cgimage(cg, NSSize::new(cw, ch));
    CFRelease(cg); // NSImage retains its own copy
    if nsimg.is_null() {
        add_preview_icon_fallback(container, pw, ph, w, colors);
        add_visibility_badge_if_needed(container, pw, ph, w);
        return;
    }
    let shown: *mut AnyObject = if w.minimized {
        let grayed = grayed_image(nsimg, NSSize::new(cw, ch));
        release_obj(nsimg);
        grayed
    } else {
        nsimg
    };
    let iv: *mut AnyObject = msg_send![class!(NSImageView), alloc];
    let iv: *mut AnyObject = msg_send![iv, initWithFrame: NSRect::new(
        // For wide aspect-fit windows, keep leftover height below the image instead of
        // centering it vertically, so the thumbnail stays visually anchored at the top.
        NSPoint::new((pw - cw) / 2.0, (ph - ch).max(0.0)),
        NSSize::new(cw, ch)
    )];
    let _: () = msg_send![iv, setImage: shown];
    release_obj(shown);
    let _: () = msg_send![iv, setImageScaling: 2u64]; // exact size, no additional scaling
    let _: () = msg_send![container, addSubview: iv];
    release_obj(iv);
    add_visibility_badge_if_needed(container, pw, ph, w);
}

pub(crate) fn create_card_view(
    w: &WindowInfo,
    index: usize,
    card_width: f64,
    card_h: f64,
    thumbnail_capture_allowed: bool,
) -> *mut AnyObject {
    unsafe {
        let card_cls = CARD_CLASS.lock().unwrap().unwrap();
        let card_cls_ptr = card_cls.0 as *mut AnyObject;

        // Thumbnails on = the mockup layout (caption + 16:10 preview); off = the
        // legacy layout (centered icon + two text lines). The height comes from the
        // caller: after the flow layout's shrink step each card's actual height is
        // smaller than the base, and the internal geometry MUST be laid out from
        // that actual height or masksToBounds clips the caption away (verified).
        let use_new = crate::theme::thumbnails_enabled();
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_width, card_h));
        let view: *mut AnyObject = msg_send![card_cls_ptr, alloc];
        let view: *mut AnyObject = msg_send![view, initWithFrame: frame];

        // Enable layer for selection border
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        // 16px radius for the new layout (.item), legacy keeps 14.
        let _: () = msg_send![layer, setCornerRadius: if use_new { 16.0f64 } else { 14.0f64 }];
        // masksToBounds MUST be false: the selected-state shadow draws OUTSIDE the
        // card bounds and clipping would swallow it entirely. Children (preview /
        // caption / close button) are all inset from the card edges and stay inside
        // the rounded shape, so disabling the clip is safe (cornerRadius still rounds
        // the background and border).
        let _: () = msg_send![layer, setMasksToBounds: false];

        // Store card index in side map (avoids msg_send! issues on dynamic classes)
        set_card_index(view, index);
        set_card_key(view, (w.pid, w.window_id));
        set_card_signature(
            view,
            card_signature(w, frame, use_new, thumbnail_capture_allowed),
        );

        let colors = current_colors();

        if use_new {
            // The mockup's first box-shadow is a zero-blur soft-blue ring spread 2pt
            // outward. A transparent NSImageView carries a 2pt border: expanding its
            // frame by 2pt per side makes the inward-drawn border cover exactly the
            // card's outer [-2,0] band. It is added before caption/preview content;
            // masksToBounds=false keeps the outer ring visible. refresh_highlight owns
            // visibility and theme color.
            let ring_frame = NSRect::new(
                NSPoint::new(-2.0, -2.0),
                NSSize::new(card_width + 4.0, card_h + 4.0),
            );
            let ring: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let ring: *mut AnyObject = msg_send![ring, initWithFrame: ring_frame];
            let _: () = msg_send![ring, setTag: THUMB_SELECTION_RING_TAG];
            let _: () = msg_send![ring, setWantsLayer: true];
            let ring_layer: *mut AnyObject = msg_send![ring, layer];
            let _: () = msg_send![ring_layer, setCornerRadius: 18.0f64];
            let _: () = msg_send![ring_layer, setMasksToBounds: false];
            let _: () = msg_send![ring_layer, setBorderWidth: 2.0f64];
            layer_set_border(
                ring_layer,
                hex_to_cg_color(color_with_alpha(
                    colors.card_border_sel,
                    SELECTION_RING_ALPHA,
                )),
            );
            let _: () = msg_send![ring, setHidden: true];
            let _: () = msg_send![view, addSubview: ring];
            release_obj(ring);

            let caption_h = thumb_caption_h();
            let preview_h = thumb_preview_h(card_h);
            let caption_y = card_h - THUMB_PAD - caption_h;

            let mini_sz = (22.0 * text_scale())
                .clamp(16.0, 30.0)
                .min((caption_h - 2.0).max(1.0));
            let mini_frame = NSRect::new(
                NSPoint::new(THUMB_PAD, caption_y + (caption_h - mini_sz) / 2.0),
                NSSize::new(mini_sz, mini_sz),
            );
            let mut mini_img: Option<*mut AnyObject> = None;
            if let Some(ref icon_path) = w.icon_path {
                let ns_path = make_nsstring(icon_path);
                let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
                let img: *mut AnyObject = msg_send![img, initWithContentsOfFile: ns_path];
                CFRelease(ns_path as *const c_void);
                if !img.is_null() {
                    mini_img = Some(img);
                }
            }
            let mini: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let mini: *mut AnyObject = msg_send![mini, initWithFrame: mini_frame];
            let _: () = msg_send![mini, setWantsLayer: true];
            let ml: *mut AnyObject = msg_send![mini, layer];
            let _: () = msg_send![ml, setCornerRadius: 5.0f64];
            let _: () = msg_send![ml, setMasksToBounds: true];
            match mini_img {
                Some(img) => {
                    let _: () = msg_send![mini, setImage: img];
                    release_obj(img);
                    let _: () = msg_send![mini, setImageScaling: 3u64];
                }
                None => {
                    layer_set_background(ml, hex_to_cg_color(colors.icon_inner_bg));
                    let init_char = w.app_name.chars().next().unwrap_or('?').to_string();
                    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: (10.0 * text_scale()).clamp(8.0, 16.0), weight: 0.5f64];
                    let label = make_centered_label(
                        &init_char,
                        font,
                        hex_to_ns_color(colors.icon_text),
                        0.0,
                        mini_sz,
                        mini_sz,
                    );
                    let _: () = msg_send![mini, addSubview: label];
                    release_obj(label);
                }
            }
            let _: () = msg_send![view, addSubview: mini];
            release_obj(mini);

            // --- Title (left-aligned, tail-truncated; the app name sinks into the
            // status footer) ---
            let title_x = THUMB_PAD + mini_sz + THUMB_CAPTION_ICON_GAP;
            let close_sz = 24.0;
            let title_w = (card_width - title_x - close_sz - THUMB_PAD).max(20.0);
            let title_size = card_title_font_size();
            let title_font: *mut AnyObject = {
                let cfg = CONFIG.read().unwrap();
                msg_send![class!(NSFont), systemFontOfSize: title_size, weight: cfg.fonts.title_weight]
            };
            let title_label = make_left_label(
                &card_caption(
                    &w.window_title,
                    &w.app_name,
                    crate::theme::show_app_name_in_cards(),
                ),
                title_font,
                hex_to_ns_color(colors.win_title),
                title_x,
                caption_y + 2.0,
                title_w,
                (caption_h - 4.0).max(1.0),
            );
            let _: () = msg_send![view, addSubview: title_label];
            release_obj(title_label);

            let preview_frame = NSRect::new(
                NSPoint::new(THUMB_PAD, THUMB_PAD),
                NSSize::new(card_width - THUMB_PAD * 2.0, preview_h),
            );
            // The preview container is an NSImageView: refresh_highlight's
            // selected-state nudge locates it via viewWithTag, and setTag: only
            // exists on NSControl-derived classes -- a bare NSView's tag property is
            // readonly and objc2's debug check would panic (same pitfall as the
            // letter avatar). An image-less NSImageView draws nothing; subviews
            // render normally.
            let container: *mut AnyObject = msg_send![class!(NSImageView), alloc];
            let container: *mut AnyObject = msg_send![container, initWithFrame: preview_frame];
            let _: () = msg_send![container, setTag: THUMB_PREVIEW_TAG];
            let _: () = msg_send![container, setWantsLayer: true];
            let cl: *mut AnyObject = msg_send![container, layer];
            // Preview corner radius of 4pt: the preview is a shrunken window (~1/8
            // scale), where a real macOS window corner (Tahoe 16pt / Sequoia 10pt)
            // equates to ~2pt; use 4pt as the clipping radius.
            let _: () = msg_send![cl, setCornerRadius: 4.0f64];
            let _: () = msg_send![cl, setMasksToBounds: true];
            // Keep the preview container transparent and borderless so leftover space below a
            // thumbnail shows the switcher's glass background directly.
            let _: () = msg_send![cl, setBorderWidth: 0.0f64];
            layer_set_background(cl, std::ptr::null_mut());

            populate_thumbnail_preview(container, w, &colors, thumbnail_capture_allowed);

            let _: () = msg_send![view, addSubview: container];
            release_obj(container); // view owns the container; drop our alloc +1
        } else {
            let icon_x = (card_width - icon_px()) / 2.0; // 16.0
            let icon_bottom = card_h - 8.0 - icon_px(); // 64.0

            if let Some(ref icon_path) = w.icon_path {
                let ns_path = make_nsstring(icon_path);
                let ns_image: *mut AnyObject = msg_send![class!(NSImage), alloc];
                let ns_image: *mut AnyObject = msg_send![ns_image, initWithContentsOfFile: ns_path];
                CFRelease(ns_path as *const c_void);

                if !ns_image.is_null() {
                    let img_frame = NSRect::new(
                        NSPoint::new(icon_x, icon_bottom),
                        NSSize::new(icon_px(), icon_px()),
                    );
                    let img_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
                    let img_view: *mut AnyObject = msg_send![img_view, initWithFrame: img_frame];
                    // Minimized: bake a grayed version (gray confined to the icon's alpha, no box); else original.
                    let image_to_show: *mut AnyObject = if w.minimized {
                        let g = grayed_image(ns_image, NSSize::new(icon_px(), icon_px()));
                        release_obj(ns_image); // original no longer needed
                        g
                    } else {
                        ns_image
                    };
                    let _: () = msg_send![img_view, setImage: image_to_show];
                    release_obj(image_to_show); // img_view owns the image now; drop our alloc +1
                                                // NSImageScaleProportionallyUpOrDown = 3
                    let _: () = msg_send![img_view, setImageScaling: 3u64];
                    let _: () = msg_send![img_view, setTag: ICON_VIEW_TAG];
                    let _: () = msg_send![view, addSubview: img_view];
                    release_obj(img_view); // view owns the image view now; drop our alloc +1
                }
            } else {
                // Letter icon: rounded square with first letter
                let letter_sq = letter_px();
                let letter_x = icon_x + (icon_px() - letter_sq) / 2.0;
                // Center the 64x64 square within the 128x128 icon area
                let letter_y = icon_bottom + (icon_px() - letter_sq) / 2.0;
                let letter_frame = NSRect::new(
                    NSPoint::new(letter_x, letter_y),
                    NSSize::new(letter_sq, letter_sq),
                );

                // The letter-avatar container uses NSImageView: the refresh path locates it via
                // viewWithTag, and setTag: only exists on NSControl-derived classes -- a bare
                // NSView's tag property is readonly and objc2's debug check would panic.
                let letter_view: *mut AnyObject = msg_send![class!(NSImageView), alloc];
                let letter_view: *mut AnyObject =
                    msg_send![letter_view, initWithFrame: letter_frame];
                let _: () = msg_send![letter_view, setWantsLayer: true];
                let _: () = msg_send![letter_view, setTag: ICON_VIEW_TAG];
                let ll: *mut AnyObject = msg_send![letter_view, layer];
                let _: () = msg_send![ll, setCornerRadius: 14.0f64];
                let _: () = msg_send![ll, setMasksToBounds: true];
                let bg_color = hex_to_cg_color(colors.icon_inner_bg);
                layer_set_background(ll, bg_color);

                let init = w.app_name.chars().next().unwrap_or('?').to_string();
                let font: *mut AnyObject =
                    msg_send![class!(NSFont), systemFontOfSize: 28.0f64, weight: 0.4f64];
                let text_color = hex_to_ns_color(colors.icon_text);
                let label = make_centered_label(&init, font, text_color, 0.0, letter_sq, letter_sq);
                let _: () = msg_send![letter_view, addSubview: label];
                release_obj(label); // letter_view owns the label; drop our alloc +1
                let _: () = msg_send![view, addSubview: letter_view];
                release_obj(letter_view); // view owns the letter view; drop our alloc +1
                if w.minimized {
                    // Minimized window: overlay a light wash on the letter icon (radius matches the bg).
                    let dim: *mut AnyObject = msg_send![class!(NSView), alloc];
                    let dim: *mut AnyObject = msg_send![dim, initWithFrame: letter_frame];
                    let _: () = msg_send![dim, setWantsLayer: true];
                    let dl: *mut AnyObject = msg_send![dim, layer];
                    let _: () = msg_send![dl, setCornerRadius: 14.0f64];
                    let _: () = msg_send![dl, setMasksToBounds: true];
                    layer_set_background(dl, hex_to_cg_color(0x808080AA));
                    let _: () = msg_send![view, addSubview: dim];
                    release_obj(dim);
                }
            }

            // Gap below icon before text starts
            let text_gap: f64 = 6.0;
            // Primary line = window title, secondary = app name: title 12px medium
            // (win_title), app name 10px regular (app_name).
            let text_scale = text_scale();
            let primary_line_h = 18.0 * text_scale;
            let secondary_line_h = 16.0 * text_scale;
            let primary_bottom = icon_bottom - text_gap - primary_line_h;
            // Secondary line: 16px tall at the bottom.
            let secondary_bottom = primary_bottom - 2.0 - secondary_line_h;

            // --- Primary line: window title (12px medium, dark).
            let primary_font_size = card_title_font_size();
            let primary_font: *mut AnyObject = {
                let cfg = CONFIG.read().unwrap();
                msg_send![class!(NSFont), systemFontOfSize: primary_font_size, weight: cfg.fonts.title_weight]
            };
            let primary_color = hex_to_ns_color(colors.win_title);
            let title_label = make_centered_label(
                display_title(&w.window_title, &w.app_name),
                primary_font,
                primary_color,
                primary_bottom,
                card_width,
                primary_line_h,
            );
            let _: () = msg_send![view, addSubview: title_label];
            release_obj(title_label); // view owns the label; drop our alloc +1

            // --- Secondary line: app name (10px regular, light).
            let secondary_font_size = card_app_name_font_size();
            let secondary_font: *mut AnyObject = {
                let cfg = CONFIG.read().unwrap();
                msg_send![class!(NSFont), systemFontOfSize: secondary_font_size, weight: cfg.fonts.app_name_weight]
            };
            let secondary_color = hex_to_ns_color(colors.app_name);
            let name_label = make_centered_label(
                &w.app_name,
                secondary_font,
                secondary_color,
                secondary_bottom,
                card_width,
                secondary_line_h,
            );
            let _: () = msg_send![view, addSubview: name_label];
            release_obj(name_label); // view owns the label; drop our alloc +1
        }

        // NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways
        let opts: u64 = 0x01 | 0x80;
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(card_width, card_h));
        let ta: *mut AnyObject = msg_send![ta, initWithRect: bounds, options: opts, owner: view, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![view, addTrackingArea: ta];
        release_obj(ta); // view owns the tracking area; drop our alloc +1

        // --- Close button: unified style across both layouts (20x20, radius 6,
        // font 12 -- identical to the legacy icon-mode button, hover-red included);
        // only the position follows the layout: caption-row right edge (centered)
        // in thumbnail mode, top-right corner in legacy. ---
        let (btn_frame, btn_radius, btn_font_sz) = if use_new {
            let caption_h = thumb_caption_h();
            let caption_y = card_h - THUMB_PAD - caption_h;
            (
                NSRect::new(
                    NSPoint::new(
                        card_width - THUMB_PAD - 20.0,
                        caption_y + (caption_h - 20.0) / 2.0,
                    ),
                    NSSize::new(20.0, 20.0),
                ),
                6.0f64,
                12.0f64,
            )
        } else {
            (
                NSRect::new(
                    NSPoint::new(card_width - 27.0, card_h - 27.0),
                    NSSize::new(20.0, 20.0),
                ),
                6.0f64,
                12.0f64,
            )
        };
        let btn: *mut AnyObject = msg_send![close_button_class(), alloc];
        let btn: *mut AnyObject = msg_send![btn, initWithFrame: btn_frame];
        let _: () = msg_send![btn, setBordered: false];
        let title_ns = make_nsstring("×");
        let _: () = msg_send![btn, setTitle: title_ns];
        CFRelease(title_ns as *const c_void);
        let close_font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: btn_font_sz, weight: 0.0f64];
        let _: () = msg_send![btn, setFont: close_font];
        let _: () = msg_send![btn, setAlignment: 1isize]; // NSTextAlignmentCenter on arm64
                                                          // The HTML .close base state uses a transparent background and translucent black text.
        let _: () = msg_send![btn, setWantsLayer: true];
        let bl: *mut AnyObject = msg_send![btn, layer];
        let _: () = msg_send![bl, setCornerRadius: btn_radius];
        let _: () = msg_send![bl, setMasksToBounds: true];
        set_close_button_hover_style(btn, false);
        let _: () = msg_send![btn, setTag: CLOSE_BTN_TAG];
        let _: () = msg_send![btn, setTarget: crate::CONTROLLER.lock().unwrap().unwrap().0];
        let _: () = msg_send![btn, setAction: sel!(closeCard:)];
        let _: () = msg_send![btn, setHidden: true];

        // Add a tracking area to the button itself so the red hover style only applies while
        // the pointer is over the × button.
        let opts: u64 = 0x01 | 0x80; // NSTrackingMouseEnteredAndExited | ActiveAlways
        let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        let ta: *mut AnyObject = msg_send![ta, initWithRect: NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(20.0, 20.0)
        ), options: opts, owner: btn, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![btn, addTrackingArea: ta];
        release_obj(ta);

        let _: () = msg_send![view, addSubview: btn];
        release_obj(btn); // view owns the button; drop our alloc +1

        view
    }
}

/// Pick the target screen frame for the overlay (global coords) per config:
/// - "main": always the primary display (index 0 of NSScreen.screens; the first entry is
///   guaranteed to host the menu bar).
/// - "active_window": follow the active window -- the screen containing the center of the
///   active window's bounds; falls back to the primary display when the bounds are unavailable
///   (all zeros / no windows) or the center isn't on any screen.
///
/// Note: NSScreen.mainScreen must NOT be used as "the primary screen" -- it returns the screen
/// containing the key window, so summoning while the active app sits on a secondary display
/// would resolve to that display, making "always on main screen" behave like "follow active
/// window". The primary display is screens[0].
/// Returns (target screen frame, visibleFrame, backingScaleFactor). The NSScreen object is
/// never cached across summons, so display hot-plug/unplug and scaling-mode changes use the
/// new live scale on the next summon.
pub(super) fn cg_window_center_to_appkit_point(
    bounds: (f64, f64, f64, f64),
    primary_frame: NSRect,
) -> NSPoint {
    let (bx, by, bw, bh) = bounds;
    let primary_top = primary_frame.origin.y + primary_frame.size.height;
    // CG's y origin is at the primary display's top, while AppKit's is at its bottom;
    // the conversion must use the primary display's top edge as the shared baseline.
    NSPoint::new(bx + bw / 2.0, primary_top - (by + bh / 2.0))
}

fn overlay_target_screen(windows: &[WindowInfo]) -> (NSRect, NSRect, f64) {
    unsafe {
        let metrics = |screen: *mut AnyObject| {
            let frame: NSRect = msg_send![screen, frame];
            let visible: NSRect = msg_send![screen, visibleFrame];
            let scale: f64 = msg_send![screen, backingScaleFactor];
            (frame, visible, if scale > 0.0 { scale } else { 1.0 })
        };
        let pos = CONFIG.read().unwrap().windows.overlay_position.clone();
        // Primary display = screens[0] (first entry hosts the menu bar); fall back to
        // mainScreen if the screens array is somehow empty.
        let main_screen_obj: *mut AnyObject = {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            let count: usize = msg_send![screens, count];
            if count > 0 {
                // objectAtIndex: expects a 'q' (signed long) argument; pass isize/i64 or
                // objc2's runtime encoding check panics on an i32 literal.
                msg_send![screens, objectAtIndex: 0isize]
            } else {
                msg_send![class!(NSScreen), mainScreen]
            }
        };
        if pos != "active_window" {
            return metrics(main_screen_obj);
        }
        // The active window: after collect_windows' sort, index 0 is the frontmost (is_active set).
        let Some(active) = windows.iter().find(|w| w.is_active) else {
            return metrics(main_screen_obj);
        };
        let primary_frame: NSRect = msg_send![main_screen_obj, frame];
        let (_, _, bw, bh) = active.bounds;
        // All-zero bounds = unavailable, can't locate, fall back to the main screen.
        if bw <= 0.0 || bh <= 0.0 {
            return metrics(main_screen_obj);
        }
        let center = cg_window_center_to_appkit_point(active.bounds, primary_frame);
        // Iterate all screens, find the one containing the active window's center.
        let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        let mut i = 0usize;
        while i < count {
            // Same as line 934: objectAtIndex: wants 'q'; usize ('Q') would fail the check too.
            let s: *mut AnyObject = msg_send![screens, objectAtIndex: i as isize];
            let f: NSRect = msg_send![s, frame];
            if center.x >= f.origin.x
                && center.x <= f.origin.x + f.size.width
                && center.y >= f.origin.y
                && center.y <= f.origin.y + f.size.height
            {
                return metrics(s);
            }
            i += 1;
        }
        metrics(main_screen_obj)
    }
}

/// Find the widest card represented by a currently maximized window, and use it as
/// the cap for unusually wide window thumbnails.
fn thumbnail_max_card_width(
    windows: &[WindowInfo],
    card_h: f64,
    max_inner: f64,
    screen_frame: NSRect,
    screen_visible: NSRect,
) -> f64 {
    let fallback = thumb_card_w_for_aspect(card_h, THUMB_PREVIEW_RATIO).min(max_inner);
    let screen_w = screen_frame.size.width.max(1.0);
    let visible_h = screen_visible.size.height.max(1.0);

    windows
        .iter()
        .filter_map(|window| {
            let (_, _, width, height) = window.bounds;
            // A maximized window should nearly fill the screen width and the usable height.
            // This deliberately avoids treating half-screen and ordinary resized windows as
            // the reference, even when their aspect ratio is unusually wide.
            let maximized = width >= screen_w * 0.90 && height >= visible_h * 0.80;
            maximized.then(|| thumb_card_w_for_aspect(card_h, width / height).min(max_inner))
        })
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(fallback)
}

#[derive(Default)]
struct CardReconcileStats {
    reused: usize,
    created: usize,
    replaced: usize,
    removed: usize,
}

/// Reconcile the persistent card document against the latest window snapshot. Cards are keyed
/// by `(pid, window_id)` rather than their current MRU index, so a reorder only updates frames
/// and side-map indices. A card is rebuilt only when its content or geometry signature changes.
unsafe fn reconcile_card_views(
    document: *mut AnyObject,
    windows: &[WindowInfo],
    placements: &[CardPlacementFrame],
    card_height: f64,
    thumbnail_capture_allowed: bool,
) -> CardReconcileStats {
    let use_new = crate::theme::thumbnails_enabled();
    let mut existing: HashMap<WindowKey, *mut AnyObject> = card_views(document)
        .into_iter()
        .filter_map(|card| card_key(card).map(|key| (key, card)))
        .collect();
    let mut stats = CardReconcileStats::default();

    for &(idx, card_x, card_y, card_w) in placements {
        let Some(window) = windows.get(idx) else {
            continue;
        };
        let key = (window.pid, window.window_id);
        let desired_frame = NSRect::new(
            NSPoint::new(card_x, card_y - status_h()),
            NSSize::new(card_w, card_height),
        );
        let desired_signature =
            card_signature(window, desired_frame, use_new, thumbnail_capture_allowed);

        let card = existing.remove(&key);
        let action = match card {
            None => CardReconcileAction::Create,
            Some(card) => {
                let signature = card_signature_for(card);
                card_reconcile_action(signature.as_ref(), &desired_signature)
            }
        };
        match action {
            CardReconcileAction::Create => {
                let card = create_card_view(
                    window,
                    idx,
                    desired_frame.size.width,
                    desired_frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![card, setFrame: desired_frame];
                let _: () = msg_send![document, addSubview: card];
                release_obj(card);
                stats.created += 1;
            }
            CardReconcileAction::Reuse => {
                let card = card.unwrap();
                set_card_index(card, idx);
                let _: () = msg_send![card, setFrame: desired_frame];
                if use_new && thumbnail_capture_allowed {
                    crate::thumbnail::touch_cached_frame(window.pid, window.window_id);
                }
                stats.reused += 1;
            }
            CardReconcileAction::Replace => {
                let card = card.unwrap();
                remove_card_index(card);
                let new_card = create_card_view(
                    window,
                    idx,
                    desired_frame.size.width,
                    desired_frame.size.height,
                    thumbnail_capture_allowed,
                );
                let _: () = msg_send![new_card, setFrame: desired_frame];
                let _: () = msg_send![card, removeFromSuperview];
                let _: () = msg_send![document, addSubview: new_card];
                release_obj(new_card);
                stats.replaced += 1;
            }
        }
    }

    for (_, card) in existing {
        remove_card_index(card);
        let _: () = msg_send![card, removeFromSuperview];
        stats.removed += 1;
    }
    stats
}

pub(crate) fn show_overlay() {
    if card_close_in_progress() {
        // Keep the existing view tree stable during close reflow to avoid rebuild flicker.
        return;
    }
    unsafe {
        // TIMING-DEBUG stage timing: locate summon stalls (card build / icons / resize / status bar).
        let t0 = Instant::now();
        let windows = with_tab_state(|state_opt| {
            state_opt
                .as_ref()
                .map(|state| state.windows.clone())
                .unwrap_or_default()
        });

        let window = OVERLAY_WINDOW.lock().unwrap().unwrap().0;
        let container = CONTAINER.lock().unwrap().unwrap().0;
        let document = CARD_DOCUMENT.lock().unwrap().unwrap().0;

        // The target screen comes first: the flow layout needs its width as the
        // packing budget; centering reuses it.
        let (screen_frame, screen_visible, screen_scale) = overlay_target_screen(&windows);
        let use_flow = crate::theme::thumbnails_enabled();
        // One TCC preflight covers every card in this render instead of querying once
        // per create_card_view call.
        let thumbnail_capture_allowed = use_flow && crate::thumbnail::capture_allowed();

        // Both modes use the same complete-document plus scrollable-viewport model.
        // Height uses the target screen's complete visibleFrame; with fewer rows,
        // panel_h still shrinks to the natural row count, and only larger content scrolls.
        // PANEL_MARGIN above and below comes out of the height budget, so step selection lands on a
        // smaller step (0.8/0.75) instead of the panel crossing the menu bar. The sides keep no
        // margin: the panel hugs its content width anyway.
        let max_panel_h =
            (screen_visible.size.height * PANEL_MAX_HEIGHT_RATIO - 2.0 * PANEL_MARGIN).max(240.0);
        let scroll_offset = *THUMB_SCROLL_OFFSET.lock().unwrap();
        let layout = if use_flow {
            // Thumbnails balance rows by window aspect; icon-only mode uses fixed cards and auto columns.
            // The width budget also comes from visibleFrame: a side Dock reserves part of the frame,
            // and using the full frame lets the panel cover it (the height side already did this).
            let screen_inner = (screen_visible.size.width - H_PADDING * 2.0).max(160.0);
            let max_panel_w =
                (screen_visible.size.width * PANEL_MAX_WIDTH_RATIO).max(160.0 + H_PADDING * 2.0);
            let max_inner = (max_panel_w - H_PADDING * 2.0 - THUMB_SCROLLBAR_W)
                .min(screen_inner)
                .max(160.0);
            // The card step comes from the available panel (see theme::thumb_scale_for_panel)
            // instead of a window-count table, so the width cap has to be derived from whichever
            // step the layout picks; the closure does that conversion.
            let aspects: Vec<f64> = windows
                .iter()
                .map(|wi| {
                    let (_, _, bw, bh) = wi.bounds;
                    if bw > 0.0 && bh > 0.0 {
                        bw / bh
                    } else {
                        THUMB_PREVIEW_RATIO
                    }
                })
                .collect();
            plan_thumb_scroll_layout_with_max_card_w(
                &aspects,
                max_inner,
                max_panel_w,
                max_panel_h,
                THUMB_ROW_GAP,
                THUMB_SCROLLBAR_W,
                scroll_offset,
                |card_h| {
                    thumbnail_max_card_width(
                        &windows,
                        card_h,
                        max_inner,
                        screen_frame,
                        screen_visible,
                    )
                },
            )
        } else {
            plan_icon_scroll_layout(
                windows.len(),
                screen_visible.size.width,
                max_panel_h,
                THUMB_SCROLLBAR_W,
                scroll_offset,
            )
        };
        *THUMB_VISIBLE_RANGE.lock().unwrap() = Some(layout.visible.clone());
        *THUMB_ROW_RANGES.lock().unwrap() = Some(layout.row_ranges.clone());
        *THUMB_MAX_ROWS.lock().unwrap() = layout.max_rows.max(1);
        // The post-close reflow reuses the same inset/budget/teaser verdict, or closing a card makes
        // the panel taller (measured).
        *THUMB_CONTENT_INSET.lock().unwrap() = layout.content_inset;
        *THUMB_PANEL_MAX_H.lock().unwrap() = max_panel_h;
        *THUMB_TEASER_FITS.lock().unwrap() = crate::theme::thumb_teaser_fits(
            layout.card_h,
            max_panel_h,
            if use_flow {
                THUMB_ROW_GAP
            } else {
                ICON_CARD_GAP
            },
        );
        *THUMB_SCROLL_ROW.lock().unwrap() = layout.row_start;
        *THUMB_SCROLL_OFFSET.lock().unwrap() = scroll_offset.clamp(0.0, layout.max_scroll_offset);
        *THUMB_SCROLL_MAX_OFFSET.lock().unwrap() = layout.max_scroll_offset;
        *THUMB_SCROLL_ROW_PITCH.lock().unwrap() = layout.card_h
            + if use_flow {
                THUMB_ROW_GAP
            } else {
                ICON_CARD_GAP
            };
        let thumb_scroll_metrics =
            Some((layout.overflowed, layout.row_ranges.len(), layout.max_rows));
        let h = layout.panel_h;
        let w = layout.panel_w;
        let card_h_use = layout.card_h;
        let document_h = layout.document_h;
        let card_h_outer = card_h_use;
        // Placement rule (both axes): center on the **screen frame** first, so the gaps to the screen
        // edges look symmetric, then clamp into the visible frame so the panel never crosses the menu
        // bar and never covers a visible Dock. Centering on visibleFrame alone shifts the whole panel
        // down by the top reservation (the 30pt menu bar): measured 174pt above vs 144pt below when
        // the Dock is hidden, since a hidden Dock reserves nothing at the bottom.
        let x = clamp_into_visible(
            (screen_frame.size.width - w) / 2.0 + screen_frame.origin.x,
            screen_visible.origin.x,
            screen_visible.size.width,
            w,
        );
        let y = clamp_into_visible(
            (screen_frame.size.height - h) / 2.0 + screen_frame.origin.y,
            // The clamp only uses the visible area: the margin was already subtracted from the height
            // budget, and subtracting it here too would push the panel to one side (54/36-style
            // asymmetry when the panel nearly fills the budget). The budget already guarantees
            // h <= visible - 2*margin, so screen-centering always leaves at least PANEL_MARGIN.
            screen_visible.origin.y,
            screen_visible.size.height,
            h,
        );
        // Include the panel frame in the same log line: placement is user-visible, so A2 can assert
        // the top/bottom and left/right gaps stay symmetric.
        log_debug!(
            "[overlay] layout mode={} scale={:.2} card_h={:.0} inset={:.0} panel={:.0}x{:.0} at={:.0},{:.0} visible={}..{} of {} offset={:.1} row={} rows={} visible_rows={} overflow={}",
            if use_flow { "thumbnail" } else { "icon" },
            layout.scale,
            layout.card_h,
            layout.content_inset,
            layout.panel_w,
            layout.panel_h,
            x,
            y,
            layout.visible.start,
            layout.visible.end,
            windows.len(),
            scroll_offset,
            layout.row_start,
            layout.row_ranges.len(),
            layout.max_rows,
            layout.overflowed
        );
        // Keep every card in the document and let clip bounds define the viewport; creating only
        // visible cards would leave no later windows to reveal while scrolling.
        let placements: Vec<CardPlacementFrame> = layout
            .document_placements
            .iter()
            .map(|p| (p.index, p.x, p.y, p.width))
            .collect();
        let new_frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        // Compute capture demand only after flow layout determines the actual card height:
        // even on the same 2x screen, a small set growing from 1.0 to 1.5 can upgrade 512px
        // to 640px. Live screen scale naturally handles hot-plug; a higher cached frame remains
        // valid after returning to a lower-demand display.
        let capture_target_px_h = use_flow.then(|| {
            crate::thumbnail::target_px_height(thumb_preview_h(card_h_outer), screen_scale)
        });
        if let Some(target_px_h) = capture_target_px_h {
            *THUMB_CAPTURE_TARGET_PX_H.lock().unwrap() = target_px_h;
            log_debug!(
                "[overlay] thumbnail target_h={} preview_h={:.1}pt backing_scale={:.2}",
                target_px_h,
                thumb_preview_h(card_h_outer),
                screen_scale
            );
        }

        // Queue the selected thumbnail immediately after layout has resolved its target size,
        // before card reconciliation does any per-card AppKit work. Capture remains asynchronous;
        // this only gives the worker the earliest safe head start for the first visible frame.
        if let Some(target_px_h) = capture_target_px_h {
            crate::thumbnail::refresh_selected_for_summon(target_px_h);
        }

        let t_reconcile = Instant::now(); // TIMING-DEBUG
        let reconcile_stats = reconcile_card_views(
            document,
            &windows,
            &placements,
            card_h_outer,
            thumbnail_capture_allowed,
        );
        let reconcile_ms = t_reconcile.elapsed().as_millis(); // TIMING-DEBUG
        let t_cards_ms = t0.elapsed().as_millis(); // TIMING-DEBUG

        let _: () = msg_send![window, setFrame: new_frame, display: false];

        // wrapper / VFX view / container all have autoresizingMask = 18
        // (width + height sizable), so they resize automatically when the
        // window frame changes. Keep the clip view above the status footer.
        let _: () = msg_send![
            container,
            setFrame: NSRect::new(
                NSPoint::new(0.0, status_h()),
                NSSize::new(w, (h - status_h()).max(1.0))
            )
        ];
        let _: () = msg_send![
            document,
            setFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(w, document_h.max(1.0))
            )
        ];
        *THUMB_DOCUMENT_HEIGHT.lock().unwrap() = document_h.max(1.0);
        apply_thumbnail_clip_offset();
        if let Some((overflowed, row_count, max_rows)) = thumb_scroll_metrics {
            update_thumbnail_scroller(w, h, overflowed, row_count, max_rows);
        } else if let Some(scroller) = thumbnail_scroller() {
            let _: () = msg_send![scroller.0, setHidden: true];
        }

        // The status text must be centered AFTER the window/container resize:
        // update_status_label computes x from the container's current width; if called before
        // the resize it uses the stale width (the initial max width at launch, or the previous
        // summon's width), leaving the text off-center once the container shrinks.
        update_status_label();

        let _: () = msg_send![window, setAcceptsMouseMovedEvents: true];
        // Refresh the highlight/selection once after summoning: fresh cards start with the
        // ⌫ button hidden, so the selected card's border and ⌫ must be applied now.
        refresh_highlight();
        let _: () = msg_send![window, displayIfNeeded];

        // Show window. NSPanel + nonactivatingPanel: the panel becomes key (keyboard works)
        // WITHOUT activating our app -- do NOT call activateIgnoringOtherApps, or the settings
        // window would be raised above the active app again. App stays inactive during the
        // whole summon, so the settings window is never raised (and no stash is needed).
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];
        let _: bool = msg_send![window, makeFirstResponder: container];
        // A2 E2E: the overlay is on screen and the selection is set, so write a snapshot (only with
        // `--e2e-state=<path>`). Placed after the window is ordered front and outside every AppState
        // borrow, so it can safely borrow the state once more to read it.
        crate::e2e_state::record("summon");
        // Start the hover poll: while shown, read the global cursor every 16ms to hit-test
        // (moves while a side button is held can't be seen via taps/tracking; polling is
        // the only reliable source).
        start_hover_timer();

        // When the app is inactive, NSView mouseMoved: may not be delivered even to the key
        // panel, so add an activeAlways tracking area (mouseMoved|activeAlways|inVisibleRect)
        // to the container to guarantee the MOUSE_MOVED gate flips -- otherwise hover selection
        // never enables. Same approach as BetterCmdTab's SwitcherView (.mouseMoved + .activeAlways).
        // When the app is inactive, NSView mouseMoved: may not be delivered even to the key
        // panel, so add an activeAlways tracking area (mouseMoved|activeAlways|inVisibleRect)
        // to the container to guarantee the MOUSE_MOVED gate flips -- otherwise hover selection
        // never enables. Same approach as BetterCmdTab's SwitcherView (.mouseMoved + .activeAlways).
        // Clear stale tracking areas first (adding on every summon piles them up and old
        // ones can go stale, killing mouseMoved delivery -- verified: some summons had no
        // hover response at all).
        let old_areas: *mut AnyObject = msg_send![container, trackingAreas];
        let old_cnt: usize = msg_send![old_areas, count];
        for i in 0..old_cnt {
            let area: *mut AnyObject = msg_send![old_areas, objectAtIndex: i];
            let _: () = msg_send![container, removeTrackingArea: area];
        }
        let mm_ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        // NSTrackingMouseEnteredAndExited=0x01 | NSTrackingMouseMoved=0x02 |
        // 0x04 = mouseDragged: while a side button is physically held (the system may still
        // treat moves as drags after the tap swallowed the down) moves still arrive;
        // 0x02 = mouseMoved; 0x80 = activeAlways; 0x200 = inVisibleRect.
        let mm_opts: u64 = 0x01 | 0x02 | 0x04 | 0x80 | 0x200;
        let container_bounds: NSRect = msg_send![container, bounds];
        let mm_ta: *mut AnyObject = msg_send![mm_ta, initWithRect: container_bounds, options: mm_opts, owner: container, userInfo: std::ptr::null::<AnyObject>()];
        let _: () = msg_send![container, addTrackingArea: mm_ta];
        release_obj(mm_ta); // container owns the tracking area; drop our alloc +1

        // Highlight selected card
        refresh_highlight();
        let t_resize_ms = t0.elapsed().as_millis(); // TIMING-DEBUG

        // Backfill missing icons (apps not cached at startup / whose launch-notification extract
        // failed, e.g. LinearMouse when its icon wasn't ready yet). Runs on every summon instead of
        // only on repeated Tab while visible -- otherwise such apps show the letter placeholder
        // until the user happens to press Tab again. Successful extracts rebuild cards in place.
        let t_icons = Instant::now(); // TIMING-DEBUG
        extract_uncached_icons();
        // Summon-time thumbnail refresh: cards already render their cached frames
        // (icon fallback when absent); stale/missing ones are re-captured async and
        // swapped in place via thumbnailReady -> rebuild_cards.
        if let Some(target_px_h) = capture_target_px_h {
            crate::thumbnail::refresh_for_summon(target_px_h);
        }
        // TIMING-DEBUG summary: per-stage timings (for chasing summon stalls).
        let total_ms = t0.elapsed().as_millis();
        log_debug!(
            "[overlay] show: reconcile={}ms reused={} created={} replaced={} removed={} layout+reconcile={}ms resize+status+highlight={}ms icons={}ms total={}ms",
            reconcile_ms,
            reconcile_stats.reused,
            reconcile_stats.created,
            reconcile_stats.replaced,
            reconcile_stats.removed,
            t_cards_ms,
            t_resize_ms - t_cards_ms,
            t_icons.elapsed().as_millis(),
            total_ms
        );
    }
}

/// Clamp a "centered on the screen" coordinate into the visible area: the gaps to the screen edges
/// stay symmetric while the panel cannot cross the menu bar or a visible Dock. When the panel is
/// larger than the visible area the legal range degenerates (lo > hi); pin it to the visible origin
/// so it never crosses the menu bar in the other direction either.
fn clamp_into_visible(ideal: f64, visible_origin: f64, visible_size: f64, size: f64) -> f64 {
    let lo = visible_origin;
    let hi = (visible_origin + visible_size - size).max(lo);
    ideal.clamp(lo, hi)
}

#[cfg(test)]
mod placement_tests {
    use super::clamp_into_visible;
    use crate::theme::PANEL_MARGIN;

    #[test]
    fn panel_margin_keeps_a_gap_above_and_below() {
        // Measured on the built-in display: 1470x956 (923 visible). The margin comes from the height
        // budget, while the clamp only uses the visible area, so the gaps stay symmetric even when the
        // panel nearly fills the budget.
        // Both screens are checked at "panel exactly at the budget" (budget = visible - 2*margin).
        for (frame_h, visible_h, panel_h) in [
            (956.0, 923.0, 923.0 - 2.0 * PANEL_MARGIN),
            (1080.0, 1050.0, 1050.0 - 2.0 * PANEL_MARGIN),
        ] {
            let y = clamp_into_visible((frame_h - panel_h) / 2.0, 0.0, visible_h, panel_h);
            let bottom_gap = y;
            let top_gap = frame_h - (y + panel_h);
            assert!(
                bottom_gap >= PANEL_MARGIN - 1e-9,
                "panel {panel_h}: bottom gap {bottom_gap} must be at least {PANEL_MARGIN}"
            );
            assert!(
                top_gap >= PANEL_MARGIN - 1e-9,
                "panel {panel_h}: top gap {top_gap} must be at least {PANEL_MARGIN}"
            );
            assert!(
                (top_gap - bottom_gap).abs() <= 1.0,
                "panel {panel_h}: top and bottom gaps must be nearly symmetric ({top_gap} vs {bottom_gap})"
            );
        }
    }

    #[test]
    fn placement_centers_on_the_screen_and_stays_inside_the_visible_area() {
        // The measured 1920x1080 external display: a 30pt menu bar and no bottom reservation
        // (hidden Dock).
        let (visible_origin, visible_size, frame_size) = (0.0, 1050.0, 1080.0);
        // A 762pt panel centered on the screen: 159pt above and below, instead of differing by 30.
        let y = clamp_into_visible(
            (frame_size - 762.0) / 2.0,
            visible_origin,
            visible_size,
            762.0,
        );
        assert!((y - 159.0).abs() < 1e-9);
        // The menu bar is still respected: the panel's top edge never passes the visible top.
        assert!(y + 762.0 <= visible_origin + visible_size + 1e-9);
        // With a visible bottom Dock (80pt reserved) and a panel that fits: keep the screen-centered
        // position instead of shifting it for the Dock.
        let centered = clamp_into_visible(
            (frame_size - 762.0) / 2.0,
            visible_origin + 80.0,
            visible_size - 80.0,
            762.0,
        );
        assert!((centered - 159.0).abs() < 1e-9);
        assert!(
            centered >= visible_origin + 80.0,
            "must not overlap the Dock"
        );
        // When it cannot fit (the ideal position lands inside the Dock strip) it is pushed above the
        // Dock instead.
        let pushed = clamp_into_visible(0.0, visible_origin + 80.0, visible_size - 80.0, 900.0);
        assert!((pushed - (visible_origin + 80.0)).abs() < 1e-9);
        // A panel taller than the visible area pins to the visible origin instead of crossing the
        // menu bar the other way.
        assert!((clamp_into_visible(0.0, 30.0, 300.0, 900.0) - 30.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod visibility_badge_tests {
    use super::needs_visibility_badge;

    #[test]
    fn hidden_or_minimized_windows_receive_visibility_badge() {
        assert!(needs_visibility_badge(true, false));
        assert!(needs_visibility_badge(false, true));
        assert!(needs_visibility_badge(true, true));
        assert!(!needs_visibility_badge(false, false));
    }
}
