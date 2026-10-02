//! Clipboard subsystem · text_style: text measurement and attributed-style helpers.

use super::*;

/// Row title (attributed): selected = white bold, unselected = labelColor.
pub(super) unsafe fn make_content_attributed(content: &str, kind: TextKind) -> *mut AnyObject {
    let palette = clipboard_palette();
    let key = ContentAttributedKey {
        content: content.to_owned(),
        kind: match kind {
            TextKind::Plain => 0,
            TextKind::Url => 1,
            TextKind::Code => 2,
        },
        primary_text: palette.primary_text,
        secondary_text: palette.secondary_text,
        accent: palette.accent,
    };
    if let Some(cached) = CONTENT_ATTRIBUTED_CACHE.lock().unwrap().get_mut(&key) {
        cached.last_used = next_ui_cache_recency();
        CFRetain(cached.object.0 as *const c_void);
        return cached.object.0;
    }
    let prepared_code = (kind == TextKind::Code).then(|| prepare_code_display(content, usize::MAX));
    let display_content = prepared_code
        .as_ref()
        .map(|code| code.text.as_str())
        .unwrap_or(content);
    let pstyle: *mut AnyObject = msg_send![class!(NSMutableParagraphStyle), alloc];
    let pstyle: *mut AnyObject = msg_send![pstyle, init];
    let _: () = msg_send![pstyle, setAlignment: -1isize]; // NSTextAlignmentNatural
    let _: () = msg_send![pstyle, setLineBreakMode: 0isize]; // NSLineBreakByWordWrapping
    let line_height = crate::theme::line_height(
        crate::theme::FONT_CONTROL,
        crate::theme::LINE_HEIGHT_BODY_RATIO,
    );
    let _: () = msg_send![pstyle, setMinimumLineHeight: line_height];
    let _: () = msg_send![pstyle, setMaximumLineHeight: line_height];

    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font: *mut AnyObject = match kind {
        TextKind::Code => {
            msg_send![class!(NSFont), monospacedSystemFontOfSize: crate::theme::FONT_CONTROL, weight: crate::theme::FONT_WEIGHT_REGULAR]
        }
        _ => msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CONTROL],
    };
    let color = match kind {
        TextKind::Url => crate::ffi::hex_to_ns_color(palette.accent),
        TextKind::Code => crate::ffi::hex_to_ns_color(palette.secondary_text),
        TextKind::Plain => crate::ffi::hex_to_ns_color(palette.primary_text),
    };
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let pstyle_key = make_nsstring("NSParagraphStyle");
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    let _: () = msg_send![attrs, setObject: pstyle, forKey: pstyle_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    CFRelease(pstyle_key as *const c_void);
    release_obj(pstyle);
    let ns = make_nsstring(display_content);
    let attr: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let attr: *mut AnyObject = msg_send![attr, initWithString: ns, attributes: attrs];
    CFRelease(ns as *const c_void);
    release_obj(attrs);
    if let Some(code) = &prepared_code {
        apply_visible_space_markers(attr, &code.text);
    } else {
        apply_link_color(attr, display_content, kind);
    }
    CFRetain(attr as *const c_void);
    let mut released = Vec::new();
    {
        let mut cache = CONTENT_ATTRIBUTED_CACHE.lock().unwrap();
        if let Some(old) = cache.insert(
            key,
            CachedUiObject {
                object: ObjPtr::new(attr),
                last_used: next_ui_cache_recency(),
            },
        ) {
            released.push(old.object);
        }
        if cache.len() > UI_CACHE_CAPACITY {
            let evicted_key = cache
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| key.clone());
            if let Some(evicted_key) = evicted_key {
                if let Some(evicted) = cache.remove(&evicted_key) {
                    released.push(evicted.object);
                }
            }
        }
    }
    for object in released {
        release_obj(object.0);
    }
    attr
}

/// The meta line (attributed): a 13px source-app icon (text attachment) + "app · time",
/// 10px 30% black. With source display off or no icon, it shows time only.
pub(super) unsafe fn make_meta_footer_attributed(
    entry: &ClipEntry,
    show_source: bool,
) -> *mut AnyObject {
    let total: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let empty_ns = make_nsstring("");
    let total: *mut AnyObject = msg_send![total, initWithString: empty_ns];
    CFRelease(empty_ns as *const c_void);

    // With the setting off, neither load nor attach the source icon so it hides together
    // with the source text.
    let icon = if should_show_source_icon(show_source, entry) {
        load_source_icon(entry, META_ICON)
    } else {
        std::ptr::null_mut()
    };
    if !icon.is_null() {
        // The 13px icon as a text attachment, baseline-aligned with a trailing space.
        let attachment: *mut AnyObject = msg_send![class!(NSTextAttachment), alloc];
        let attachment: *mut AnyObject = msg_send![attachment, init];
        let _: () = msg_send![attachment, setImage: icon];
        let _: () = msg_send![attachment, setBounds: NSRect::new(
            NSPoint::new(0.0, -2.0),
            NSSize::new(META_ICON, META_ICON)
        )];
        // attributedStringWithAttachment: returns a +0 (autoreleased) object; releasing it
        // over-releases and crashes on pool drain (same discipline as rebuild_search_hint).
        let att_str: *mut AnyObject = msg_send![
            class!(NSAttributedString),
            attributedStringWithAttachment: attachment
        ];
        release_obj(attachment);
        let _: () = msg_send![total, appendAttributedString: att_str];
        let sp = make_nsstring(" ");
        let sp_attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
        let sp_attr: *mut AnyObject = msg_send![sp_attr, initWithString: sp];
        CFRelease(sp as *const c_void);
        let _: () = msg_send![total, appendAttributedString: sp_attr];
        release_obj(sp_attr);
        release_obj(icon);
    }

    let meta = build_meta_text(entry, show_source);
    if !meta.is_empty() {
        let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
        let attrs: *mut AnyObject = msg_send![attrs, init];
        let font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
        let color = crate::ffi::hex_to_ns_color(clipboard_palette().muted_text);
        let font_key = make_nsstring("NSFont");
        let color_key = make_nsstring("NSColor");
        let _: () = msg_send![attrs, setObject: font, forKey: font_key];
        let _: () = msg_send![attrs, setObject: color, forKey: color_key];
        CFRelease(font_key as *const c_void);
        CFRelease(color_key as *const c_void);
        let ns = make_nsstring(&meta);
        let part: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
        let part: *mut AnyObject = msg_send![part, initWithString: ns, attributes: attrs];
        CFRelease(ns as *const c_void);
        release_obj(attrs);
        let _: () = msg_send![total, appendAttributedString: part];
        release_obj(part);
    }
    total
}

/// Load the source app's small icon (pre-scaled to `size` in points; null when uncached).
unsafe fn load_source_icon(entry: &ClipEntry, size: f64) -> *mut AnyObject {
    if entry.source_key.is_empty() {
        return std::ptr::null_mut();
    }
    let icon_path = crate::icon_cache::small_icon_path_for_key(&entry.source_key);
    let metadata = match std::fs::metadata(&icon_path) {
        Ok(metadata) => metadata,
        Err(_) => {
            let key = (entry.source_key.clone(), size.to_bits());
            if let Some(cached) = SOURCE_ICON_CACHE.lock().unwrap().remove(&key) {
                release_obj(cached.image.0);
            }
            return std::ptr::null_mut();
        }
    };
    let modified = metadata.modified().ok();
    if !metadata.is_file() {
        let key = (entry.source_key.clone(), size.to_bits());
        if let Some(cached) = SOURCE_ICON_CACHE.lock().unwrap().remove(&key) {
            release_obj(cached.image.0);
        }
        return std::ptr::null_mut();
    }

    let key = (entry.source_key.clone(), size.to_bits());
    {
        let cache = SOURCE_ICON_CACHE.lock().unwrap();
        if let Some(cached) = cache.get(&key) {
            if cached.modified == modified {
                let image = cached.image.0;
                let _: *mut AnyObject = msg_send![image, retain];
                return image;
            }
        }
    }

    if let Some(cached) = SOURCE_ICON_CACHE.lock().unwrap().remove(&key) {
        release_obj(cached.image.0);
    }
    let ns_path = make_nsstring(&icon_path);
    let img: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let img: *mut AnyObject = msg_send![img, initWithContentsOfFile: ns_path];
    CFRelease(ns_path as *const c_void);
    if !img.is_null() {
        let _: () = msg_send![img, setSize: NSSize::new(size, size)];
        SOURCE_ICON_CACHE.lock().unwrap().insert(
            key,
            CachedSourceIcon {
                image: ObjPtr::new(img),
                modified,
            },
        );
        let _: *mut AnyObject = msg_send![img, retain];
    }
    img
}

/// Compose the content button's left canvas (NSImage): only image rows get a 72x44
/// rounded thumbnail box (faint fill + inner ring, the new mockup's .image-preview);
/// text rows return null. The source icon no longer sits at the row's left -- it appears
/// as a small glyph in the meta line (see make_meta_footer_attributed).
pub(super) unsafe fn make_row_image(entry: &ClipEntry) -> *mut AnyObject {
    let Some(img) = &entry.image else {
        return std::ptr::null_mut();
    };
    if img.preview_png.is_empty() {
        return std::ptr::null_mut();
    }
    let palette = clipboard_palette();
    let key = RowImageKey {
        image_hash: img.hash,
        field_bg: palette.field_bg,
        card_border: palette.card_border,
    };
    if let Some(cached) = ROW_IMAGE_CACHE.lock().unwrap().get_mut(&key) {
        cached.last_used = next_ui_cache_recency();
        CFRetain(cached.object.0 as *const c_void);
        return cached.object.0;
    }
    let data: *mut AnyObject = msg_send![
        class!(NSData),
        dataWithBytes: img.preview_png.as_ptr() as *const c_void,
        length: img.preview_png.len()
    ];
    let im: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let im: *mut AnyObject = msg_send![im, initWithData: data];
    if im.is_null() {
        return std::ptr::null_mut();
    }
    let s: NSSize = msg_send![im, size];
    if s.width <= 0.0 || s.height <= 0.0 {
        release_obj(im);
        return std::ptr::null_mut();
    }
    // fit-contain into the 72x44 box.
    let scale = (THUMB_W / s.width).min(THUMB_H / s.height);
    let w = s.width * scale;
    let h = s.height * scale;
    let target: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let target: *mut AnyObject = msg_send![target, initWithSize: NSSize::new(THUMB_W, THUMB_H)];
    let _: () = msg_send![target, lockFocus];
    // Reuse the settings field surface for thumbnail boxes so light and dark themes share one
    // grayscale system.
    let fill = crate::ffi::hex_to_ns_color(palette.field_bg);
    let _: () = msg_send![fill, set];
    let box_path: *mut AnyObject = msg_send![
        class!(NSBezierPath),
        bezierPathWithRoundedRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(THUMB_W, THUMB_H)),
        xRadius: THUMB_R,
        yRadius: THUMB_R
    ];
    let _: () = msg_send![box_path, fill];
    // clip to the rounded rect, then draw the image.
    let _: () = msg_send![box_path, addClip];
    let dst = NSRect::new(
        NSPoint::new((THUMB_W - w) / 2.0, (THUMB_H - h) / 2.0),
        NSSize::new(w, h),
    );
    let src_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
    let op: usize = 1; // NSCompositingOperationCopy
    let _: () = msg_send![im, drawInRect: dst, fromRect: src_rect, operation: op, fraction: 1.0f64];
    // the inset ring.
    let ring = crate::ffi::hex_to_ns_color(palette.card_border);
    let _: () = msg_send![ring, set];
    let _: () = msg_send![box_path, setLineWidth: 1.0f64];
    let _: () = msg_send![box_path, stroke];
    let _: () = msg_send![target, unlockFocus];
    release_obj(im);
    CFRetain(target as *const c_void);
    let mut released = Vec::new();
    {
        let mut cache = ROW_IMAGE_CACHE.lock().unwrap();
        if let Some(old) = cache.insert(
            key,
            CachedUiObject {
                object: ObjPtr::new(target),
                last_used: next_ui_cache_recency(),
            },
        ) {
            released.push(old.object);
        }
        if cache.len() > UI_CACHE_CAPACITY {
            let evicted_key = cache
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| *key);
            if let Some(evicted_key) = evicted_key {
                if let Some(evicted) = cache.remove(&evicted_key) {
                    released.push(evicted.object);
                }
            }
        }
    }
    for object in released {
        release_obj(object.0);
    }
    target
}

/// A detail action is active only while detail is open and its owning row remains selected.
/// Keep this pure so row creation, detail open/close, and tests share one condition.
pub(super) fn detail_action_is_active(detail_visible: bool, selected: usize, row: usize) -> bool {
    detail_visible && selected != NO_SELECTION && selected == row
}

/// Draw the mockup's detail icon: a dark outlined circle plus i normally, or a dark filled
/// circle plus white i while active. Use a precolored NSImage instead of Unicode `ⓘ` so the
/// ring, dot, and stem follow the mockup independently.
pub(super) unsafe fn make_detail_action_icon(active: bool, hovered: bool) -> *mut AnyObject {
    make_detail_action_icon_with(clipboard_palette(), active, hovered)
}

/// The icon for an explicit palette, so both appearances can be rendered and checked in one run.
/// The failure this guards is mode-specific (dark mode only), and a test that only renders the
/// mode it happens to run in would miss it -- exactly how the bug reached the screen.
pub(super) unsafe fn make_detail_action_icon_for_mode(
    dark: bool,
    active: bool,
    hovered: bool,
) -> *mut AnyObject {
    make_detail_action_icon_with(crate::theme::ui_palette_for_mode(dark), active, hovered)
}

unsafe fn make_detail_action_icon_with(
    palette: crate::theme::UiPalette,
    active: bool,
    hovered: bool,
) -> *mut AnyObject {
    let image: *mut AnyObject = msg_send![class!(NSImage), alloc];
    let image: *mut AnyObject = msg_send![
        image,
        initWithSize: NSSize::new(DETAIL_ACTION_ICON, DETAIL_ACTION_ICON)
    ];
    let _: () = msg_send![image, lockFocus];
    let circle: *mut AnyObject = msg_send![
        class!(NSBezierPath),
        bezierPathWithOvalInRect: NSRect::new(
            // HTML uses a 20 × 20 viewBox with a circle at 10/10 and r=7. On our 16pt
            // canvas that is center 8 and radius 5.6; do not use the SVG's raw 7pt radius.
            NSPoint::new(2.4, 2.4),
            NSSize::new(11.2, 11.2)
        )
    ];
    // Match the pin/delete buttons' theme text colors: secondary normally, primary on hover/active.
    let icon_color = if active || hovered {
        palette.primary_text
    } else {
        palette.secondary_text
    };
    // An active icon is a *filled* chip, so both halves of that pair have to be chosen together:
    // the accent fill with `accent_text` on top (design-style §3.1). The fill used to be
    // `primary_text` while the glyph stayed `accent_text` -- 13.91:1 in light mode but, because
    // dark `primary_text` is near-white, 1.09:1 in dark mode, where the icon read as a blank disc.
    let (circle_role, glyph_role) = if active {
        (palette.accent, palette.accent_text)
    } else {
        (icon_color, icon_color)
    };
    let circle_color = crate::ffi::hex_to_ns_color(circle_role);
    let _: () = msg_send![circle_color, set];
    // Slightly strengthen the ring so its optical weight better matches the neighboring system glyphs.
    let _: () = msg_send![circle, setLineWidth: 1.25f64];
    if active {
        let _: () = msg_send![circle, fill];
    }
    let _: () = msg_send![circle, stroke];
    let glyph_color = crate::ffi::hex_to_ns_color(glyph_role);
    let _: () = msg_send![glyph_color, set];
    // Coordinates convert the SVG view's flipped axis: the dot is above the stem.
    let dot: *mut AnyObject = msg_send![
        class!(NSBezierPath),
        bezierPathWithOvalInRect: NSRect::new(NSPoint::new(7.2, 10.08), NSSize::new(1.6, 1.6))
    ];
    let _: () = msg_send![dot, fill];
    let stem: *mut AnyObject = msg_send![class!(NSBezierPath), bezierPath];
    let _: () = msg_send![stem, moveToPoint: NSPoint::new(8.0, 8.56)];
    let _: () = msg_send![stem, lineToPoint: NSPoint::new(8.0, 4.8)];
    let _: () = msg_send![stem, setLineWidth: 1.25f64];
    let _: () = msg_send![stem, setLineCapStyle: 1isize]; // NSLineCapStyleRound
    let _: () = msg_send![stem, stroke];
    let _: () = msg_send![image, unlockFocus];
    let _: () = msg_send![image, setTemplate: false];
    image
}

/// Apply the shared clipboard action-button state to its own rounded hover background.
/// never the button background.
fn is_clear_history_destructive_action(action: Sel) -> bool {
    action == sel!(clearClipboardHistory:)
        || action == sel!(clearClipboardUnpinned:)
        || action == sel!(clearClipboardAll:)
}

fn confirmation_surface_background(palette: crate::theme::UiPalette) -> u32 {
    // Use the theme card color so field_bg's darker neutral gray cannot make the confirmation
    // surface look muddy after compositing.
    let alpha = if palette.dark { 0xE0 } else { 0xEC };
    (palette.card_bg & 0xFFFF_FF00) | alpha
}

unsafe fn is_clear_confirmation_button(button: *mut AnyObject) -> bool {
    if button.is_null() {
        return false;
    }
    let Some(confirmation) = *CLEAR_HISTORY_CONFIRMATION.lock().unwrap() else {
        return false;
    };
    let parent: *mut AnyObject = msg_send![button, superview];
    parent == confirmation.surface.0
}

unsafe fn is_clear_history_action_button(button: *mut AnyObject) -> bool {
    CLEAR_HISTORY_ACTION_BUTTONS
        .lock()
        .unwrap()
        .map(|buttons| buttons.iter().any(|candidate| candidate.0 == button))
        .unwrap_or(false)
}

pub(super) unsafe fn set_clear_confirmation_button_style(button: *mut AnyObject, hovered: bool) {
    if button.is_null() {
        return;
    }
    let action: Sel = msg_send![button, action];
    let palette = clipboard_palette();
    let background = if hovered {
        // Give text buttons only a faint hover wash instead of restoring a solid destructive fill.
        (palette.hover_bg & 0xFFFF_FF00) | if palette.dark { 0x28 } else { 0x18 }
    } else {
        0x00000000
    };
    let text = if action == sel!(clearClipboardAll:) {
        palette.destructive
    } else {
        palette.secondary_text
    };
    let layer: *mut AnyObject = msg_send![button, layer];
    if !layer.is_null() {
        crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
        crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(0x00000000));
    }
    let _: () = msg_send![button, setContentTintColor: crate::ffi::hex_to_ns_color(text)];
}

pub(super) unsafe fn set_action_button_surface(button: *mut AnyObject, hovered: bool) {
    if button.is_null() {
        return;
    }
    let layer: *mut AnyObject = msg_send![button, layer];
    if layer.is_null() {
        return;
    }
    let palette = clipboard_palette();
    let action: Sel = msg_send![button, action];
    let background = if action == sel!(deleteEntry:) && hovered {
        (palette.destructive & 0xFFFF_FF00) | 0x18
    } else if is_clear_history_destructive_action(action) && hovered {
        (palette.destructive_hover & 0xFFFF_FF00) | 0x18
    } else if hovered {
        palette.hover_bg
    } else {
        0x00000000
    };
    crate::ffi::layer_set_background(layer, crate::ffi::hex_to_cg_color(background));
}

/// Replace the detail icon for its normal/hover/active state and reuse the single-button
/// rounded state styling.
pub(super) unsafe fn set_detail_action_style(button: *mut AnyObject, active: bool, hovered: bool) {
    if button.is_null() {
        return;
    }
    let icon = make_detail_action_icon(active, hovered);
    let empty = make_nsstring("");
    let _: () = msg_send![button, setTitle: empty];
    CFRelease(empty as *const c_void);
    let _: () = msg_send![button, setImage: icon];
    let _: () = msg_send![button, setImagePosition: 1isize]; // NSImageOnly
    release_obj(icon);
    set_action_button_surface(button, hovered);
}

/// Toggling detail does not rebuild the list, so refresh existing detail-action active states.
pub(super) fn refresh_detail_action_visuals() {
    let visible = detail_visible();
    let selected = picker_selection();
    let indices = ROW_VIEW_INDICES.lock().unwrap().clone();
    let views = ROW_HOVER_VIEWS.lock().unwrap().clone();
    unsafe {
        for (slot, view) in views.iter().enumerate() {
            let Some(&row) = indices.get(slot) else {
                continue;
            };
            set_detail_action_style(
                view.details.0,
                detail_action_is_active(visible, selected, row),
                false,
            );
        }
    }
}

/// A per-row action button (pin/delete): an SF Symbol icon, borderless, its alpha
/// supplied by the caller (pinned rows pass 1.0; unpinned rows pass hover/selection).
/// A hover-aware button class (an NSButton subclass overriding mouseEntered:/mouseExited:):
/// the hover style is picked by the action selector -- pin darkens with a faint fill,
/// delete/clear turn red, filters only darken. On exit the state is restored (filters go
/// through update_filter_pill_style so the active tint is never clobbered).
pub(super) unsafe fn hover_button_class() -> *mut AnyObject {
    static HOVER_BTN_CLS: OnceLock<StaticClass> = OnceLock::new();
    HOVER_BTN_CLS
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipHoverButton").unwrap();
            let superclass = class!(NSButton) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(mouseEntered:),
                hover_button_entered as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseExited:),
                hover_button_exited as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(mouseDown:),
                hover_button_mouse_down as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

/// The save-as button's hover/pressed state.
///
/// It follows its sibling icon buttons exactly: the glyph is `secondary_text` normally and
/// `primary_text` when engaged, and the surface is the shared `hover_bg`. It previously took a
/// scalar alpha on a literal black plus a literal black fill, which made the glyph 4.3x fainter
/// than its neighbours (2.33:1 against 10.10:1) and left hover feedback invisible in dark mode
/// (a 5% black fill over the dark panel measures 1.01:1).
unsafe fn set_detail_share_style(button: *mut AnyObject, engaged: bool) {
    // A non-template NSImage ignores contentTintColor, so replace the 18pt icon for each state.
    let icon = make_detail_save_icon(engaged);
    let _: () = msg_send![button, setImage: icon];
    release_obj(icon);
    set_action_button_surface(button, engaged);
}

/// Hover enter: color by action (the mockup's .action:hover / .clear-history:hover /
/// .filter:hover).
extern "C" fn hover_button_entered(_self: *mut c_void, _cmd: Sel, _event: *mut c_void) {
    unsafe {
        let b = _self as *mut AnyObject;
        let action: Sel = msg_send![b, action];
        if is_clear_confirmation_button(b) || is_clear_history_action_button(b) {
            set_clear_confirmation_button_style(b, true);
            return;
        }
        if action == sel!(detailSaveAs:) {
            set_detail_share_style(b, true);
            return;
        }
        if action == sel!(showItemDetails:) {
            let tag: isize = msg_send![b, tag];
            let active = tag >= 0
                && detail_action_is_active(detail_visible(), picker_selection(), tag as usize);
            set_detail_action_style(b, active, true);
            return;
        }
        if is_clear_history_destructive_action(action) {
            let palette = clipboard_palette();
            let c = crate::ffi::hex_to_ns_color(palette.destructive_hover);
            let _: () = msg_send![b, setContentTintColor: c];
            set_action_button_surface(b, true);
        } else if action == sel!(deleteEntry:) {
            let palette = clipboard_palette();
            let c = crate::ffi::hex_to_ns_color(palette.destructive);
            let _: () = msg_send![b, setContentTintColor: c];
            set_action_button_surface(b, true);
        } else if action == sel!(togglePin:) {
            // Pin hover darkens with a faint fill. Details use their dedicated SVG-style
            // outlined/filled states and were handled at this function's start.
            let palette = clipboard_palette();
            let c = crate::ffi::hex_to_ns_color(palette.primary_text);
            let _: () = msg_send![b, setContentTintColor: c];
            set_action_button_surface(b, true);
        } else if action == sel!(filterPillClicked:) {
            let c = crate::ffi::hex_to_ns_color(clipboard_palette().primary_text);
            let _: () = msg_send![b, setContentTintColor: c];
        }
    }
}

/// Hover exit: restore the base color; a filter only updates its own tint and never touches
/// the shared underline.
extern "C" fn hover_button_exited(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let b = _self as *mut AnyObject;
        let action: Sel = msg_send![b, action];
        if is_clear_confirmation_button(b) || is_clear_history_action_button(b) {
            set_clear_confirmation_button_style(b, false);
            return;
        }
        if (action == sel!(showItemDetails:)
            || action == sel!(deleteEntry:)
            || action == sel!(togglePin:))
            && !REBUILDING.load(Ordering::SeqCst)
        {
            // Action buttons are sibling views of the row, so leaving one does not trigger the
            // row button's mouseExited; clear row hover only after the whole row is left.
            let tag: isize = msg_send![b, tag];
            if tag >= 0 && (event.is_null() || !mouse_inside_row(event, tag as usize)) {
                clear_hover_row_if(tag as usize);
            }
        }
        if action == sel!(detailSaveAs:) {
            set_detail_share_style(b, false);
            return;
        }
        if action == sel!(filterPillClicked:) {
            let tag: isize = msg_send![b, tag];
            let active_tag = match *CLIP_FILTER.lock().unwrap() {
                ClipFilter::All => 0isize,
                ClipFilter::Text => 1,
                ClipFilter::Image => 2,
                ClipFilter::Link => 3,
                ClipFilter::Code => 4,
            };
            let palette = clipboard_palette();
            let tint = if tag == active_tag {
                palette.primary_text
            } else {
                palette.secondary_text
            };
            let c = crate::ffi::hex_to_ns_color(tint);
            let _: () = msg_send![b, setContentTintColor: c];
            return;
        }
        if action == sel!(showItemDetails:) {
            let tag: isize = msg_send![b, tag];
            let active = tag >= 0
                && detail_action_is_active(detail_visible(), picker_selection(), tag as usize);
            set_detail_action_style(b, active, false);
            return;
        }
        if action == sel!(deleteEntry:)
            || is_clear_history_destructive_action(action)
            || action == sel!(togglePin:)
        {
            set_action_button_surface(b, false);
        }
        let c = crate::ffi::hex_to_ns_color(clipboard_palette().secondary_text);
        let _: () = msg_send![b, setContentTintColor: c];
    }
}

/// The share button uses the HTML mockup's 7.5% pressed fill; all other buttons retain native
/// NSButton behavior.
/// Panic boundary for the C callback: a panic cannot unwind through an `extern "C"` frame (it
/// aborts the process), so it is contained here.
extern "C" fn hover_button_mouse_down(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    crate::callback_guard::void("hover_button_mouse_down", || unsafe {
        hover_button_mouse_down_inner(_self, _cmd, event)
    });
}

unsafe fn hover_button_mouse_down_inner(_self: *mut c_void, _cmd: Sel, event: *mut c_void) {
    unsafe {
        let button = _self as *mut AnyObject;
        let action: Sel = msg_send![button, action];
        if action == sel!(detailSaveAs:) {
            set_detail_share_style(button, true);
        }

        type MouseDown = unsafe extern "C" fn(*mut ObjcSuper, Sel, *mut c_void);
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class: class!(NSButton) as *const _ as *mut c_void,
        };
        let call: MouseDown = std::mem::transmute(objc_msgSendSuper as *const ());
        call(&mut sup, sel!(mouseDown:), event);

        if action == sel!(detailSaveAs:) {
            set_detail_share_style(button, true);
        }
    }
}

pub(super) unsafe fn make_action_button(
    title: &str,
    action: Sel,
    tag: isize,
    x: f64,
    y: f64,
    alpha: f64,
) -> *mut AnyObject {
    let b: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let b: *mut AnyObject = msg_send![
        b,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(ACTION_BTN, ACTION_H))
    ];
    let _: () = msg_send![b, setBordered: false];
    // hover fill needs a layer.
    let _: () = msg_send![b, setWantsLayer: true];
    let blayer: *mut AnyObject = msg_send![b, layer];
    let _: () = msg_send![blayer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let title_ns = make_nsstring(title);
    let _: () = msg_send![b, setTitle: title_ns];
    CFRelease(title_ns as *const c_void);
    let _: () = msg_send![b, setTag: tag];
    let _: () = msg_send![b, setTarget: row_target()];
    let _: () = msg_send![b, setAction: action];
    // Tint = the new mockup's .action 32% black; visibility is carried by alpha.
    let tint = crate::ffi::hex_to_ns_color(clipboard_palette().secondary_text);
    let _: () = msg_send![b, setContentTintColor: tint];
    let _: () = msg_send![b, setAlphaValue: alpha];
    // Initialize all three buttons through the same component state; hover callbacks then
    // change only the button under the pointer.
    set_action_button_surface(b, false);
    add_hover_tracking(b);
    b
}

/// a string's display width in points.
pub(super) fn localized_string_width(s: &str, font_size: f64) -> f64 {
    unsafe {
        let ns = make_nsstring(s);
        let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: font_size];
        let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
        let attrs: *mut AnyObject = msg_send![attrs, init];
        let font_key = make_nsstring("NSFont");
        let _: () = msg_send![attrs, setObject: font, forKey: font_key];
        CFRelease(font_key as *const c_void);
        let attr: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
        let attr: *mut AnyObject = msg_send![attr, initWithString: ns, attributes: attrs];
        let size: NSSize = msg_send![attr, size];
        CFRelease(ns as *const c_void);
        release_obj(attr);
        release_obj(attrs);
        size.width
    }
}

/// Create a filter pill (bare text; its style is refreshed centrally by
/// update_filter_pill_style according to the active filter).
pub(super) unsafe fn make_filter_pill(
    label: &str,
    tag: isize,
    x: f64,
    y: f64,
    w: f64,
) -> *mut AnyObject {
    let b: *mut AnyObject = msg_send![hover_button_class(), alloc];
    let b: *mut AnyObject = msg_send![
        b,
        initWithFrame: NSRect::new(NSPoint::new(x, y), NSSize::new(w, FILTERS_H))
    ];
    let _: () = msg_send![b, setBordered: false];
    let font: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![b, setFont: font];
    let label_ns = make_nsstring(label);
    let _: () = msg_send![b, setTitle: label_ns];
    CFRelease(label_ns as *const c_void);
    let _: () = msg_send![b, setTag: tag];
    let _: () = msg_send![b, setTarget: observer()];
    let _: () = msg_send![b, setAction: sel!(filterPillClicked:)];
    // hover darkens (the mockup's .filter:hover).
    add_hover_tracking(b);
    b
}

/// Build the clear-history confirmation card alongside the fixed header so its lower rows
/// remain interactive while it overlays the list.
#[allow(dead_code)]
unsafe fn build_clear_history_confirmation(header_strip: *mut AnyObject, anchor: NSRect) {
    let (surface_in_header, button_frames) = clear_history_confirmation_layout(anchor);
    let parent: *mut AnyObject = msg_send![header_strip, superview];
    let surface_frame: NSRect =
        msg_send![header_strip, convertRect: surface_in_header, toView: parent];
    let surface: *mut AnyObject = msg_send![class!(NSView), alloc];
    let surface: *mut AnyObject = msg_send![
        surface,
        initWithFrame: surface_frame
    ];
    let _: () = msg_send![surface, setWantsLayer: true];
    // The picker parent resizes, so keep the card pinned to its top-aligned trigger.
    let _: () = msg_send![surface, setAutoresizingMask: 8u64];
    let layer: *mut AnyObject = msg_send![surface, layer];
    let palette = clipboard_palette();
    // Use the theme card surface to match the surrounding picker.
    crate::ffi::layer_set_background(
        layer,
        crate::ffi::hex_to_cg_color(confirmation_surface_background(palette)),
    );
    crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
    let _: () = msg_send![layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
    let _: () = msg_send![layer, setMasksToBounds: true];
    let _: () = msg_send![surface, setHidden: true];
    let _: () = msg_send![surface, setAlphaValue: 0.0f64];
    let _: () = msg_send![parent, addSubview: surface];

    let labels = [
        t("clipboard.clear_confirm_unpinned"),
        t("clipboard.clear_confirm_all"),
    ];
    let actions = [sel!(clearClipboardUnpinned:), sel!(clearClipboardAll:)];
    let mut buttons = [std::ptr::null_mut(); 2];
    for i in 0..2 {
        let button: *mut AnyObject = msg_send![hover_button_class(), alloc];
        let button: *mut AnyObject = msg_send![
            button,
            initWithFrame: button_frames[i]
        ];
        let _: () = msg_send![button, setBordered: false];
        let font: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
        let _: () = msg_send![button, setFont: font];
        let title = make_nsstring(&labels[i]);
        let _: () = msg_send![button, setTitle: title];
        CFRelease(title as *const c_void);
        let _: () = msg_send![button, setTarget: observer()];
        let _: () = msg_send![button, setAction: actions[i]];
        let _: () = msg_send![button, setWantsLayer: true];
        let button_layer: *mut AnyObject = msg_send![button, layer];
        if !button_layer.is_null() {
            crate::ffi::layer_set_border(button_layer, crate::ffi::hex_to_cg_color(0x00000000));
            let _: () = msg_send![button_layer, setBorderWidth: 0.0f64];
            let _: () = msg_send![button_layer, setCornerRadius: crate::theme::RADIUS_CONTROL];
            let _: () = msg_send![button_layer, setMasksToBounds: true];
        }
        if !button_layer.is_null() {
            crate::ffi::layer_set_background(button_layer, crate::ffi::hex_to_cg_color(0x00000000));
        }
        set_clear_confirmation_button_style(button, false);
        let _: () = msg_send![button, setHidden: true];
        let _: () = msg_send![button, setAlphaValue: 0.0f64];
        add_hover_tracking(button);
        let _: () = msg_send![surface, addSubview: button];
        release_obj(button);
        buttons[i] = button;
    }
    release_obj(surface);
    *CLEAR_HISTORY_CONFIRMATION.lock().unwrap() = Some(ClearHistoryConfirmationViews {
        surface: ObjPtr::new(surface),
        unpinned: ObjPtr::new(buttons[0]),
        all: ObjPtr::new(buttons[1]),
    });
}

pub(super) unsafe fn apply_clear_history_confirmation_theme() {
    let Some(confirmation) = *CLEAR_HISTORY_CONFIRMATION.lock().unwrap() else {
        return;
    };
    let layer: *mut AnyObject = msg_send![confirmation.surface.0, layer];
    if layer.is_null() {
        return;
    }
    let palette = clipboard_palette();
    crate::ffi::layer_set_background(
        layer,
        crate::ffi::hex_to_cg_color(confirmation_surface_background(palette)),
    );
    crate::ffi::layer_set_border(layer, crate::ffi::hex_to_cg_color(palette.card_border));
    for button in [confirmation.unpinned, confirmation.all] {
        set_clear_confirmation_button_style(button.0, false);
    }
}

/// Refresh the filter styling: the active item is 78% black with a 16x2 underline below;
/// the rest are 38% black (the mockup's .filter). The underline is one shared little view
/// repositioned under the active button (created on the first style pass).
pub(super) fn update_filter_pill_style(_animate_underline: bool) {
    let active = *CLIP_FILTER.lock().unwrap();
    unsafe {
        let active_tag = match active {
            ClipFilter::All => 0isize,
            ClipFilter::Text => 1,
            ClipFilter::Image => 2,
            ClipFilter::Link => 3,
            ClipFilter::Code => 4,
        };
        let mut active_frame: Option<NSRect> = None;
        let palette = clipboard_palette();
        let pills = FILTER_PILLS.lock().unwrap();
        for p in pills.iter() {
            let tag: isize = msg_send![p.0, tag];
            let color: *mut AnyObject = if tag == active_tag {
                active_frame = Some(msg_send![p.0, frame]);
                crate::ffi::hex_to_ns_color(palette.primary_text)
            } else {
                crate::ffi::hex_to_ns_color(palette.secondary_text)
            };
            let _: () = msg_send![p.0, setContentTintColor: color];
        }
        drop(pills);
        // The underline: 16x2, radius 2, 45% black, 9px under the active item's text.
        if let Some(frame) = active_frame {
            let parent: *mut AnyObject = {
                let p0 = FILTER_PILLS.lock().unwrap()[0];
                msg_send![p0.0, superview]
            };
            let ux = frame.origin.x + (frame.size.width - FILTER_UNDERLINE_W) / 2.0;
            // Flipped coords: the button is 38pt tall with centered text; the underline
            // sits 9px under the text ≈ 3pt above the row's bottom.
            let uy = frame.origin.y + FILTERS_H - 3.0;
            let mut guard = FILTER_UNDERLINE.lock().unwrap();
            if let Some(u) = *guard {
                let target_frame = NSRect::new(
                    NSPoint::new(ux, uy),
                    NSSize::new(FILTER_UNDERLINE_W, FILTER_UNDERLINE_H),
                );
                let _: () = msg_send![u.0, setFrame: target_frame];
            } else {
                let u: *mut AnyObject = msg_send![class!(NSView), alloc];
                let u: *mut AnyObject = msg_send![
                    u,
                    initWithFrame: NSRect::new(
                        NSPoint::new(ux, uy),
                        NSSize::new(FILTER_UNDERLINE_W, FILTER_UNDERLINE_H)
                    )
                ];
                let _: () = msg_send![u, setWantsLayer: true];
                let ulayer: *mut AnyObject = msg_send![u, layer];
                crate::ffi::layer_set_background(
                    ulayer,
                    crate::ffi::hex_to_cg_color(palette.secondary_text),
                );
                let _: () = msg_send![ulayer, setCornerRadius: FILTER_UNDERLINE_H / 2.0];
                let _: () = msg_send![parent, addSubview: u];
                release_obj(u);
                *guard = Some(ObjPtr::new(u));
            }
        }
    }
}

/// the footer's entry-count label.
static FOOTER_COUNT: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

/// One shortcut legend (keycap + its label), kept so the layout validator can measure what the
/// footer actually reserved, instead of recomputing it from the constants that built it.
struct FooterLegend {
    cap: ObjPtr,
    key_label: ObjPtr,
    hint: ObjPtr,
}

/// The legends of the live footer, left-to-right. Empty when no footer has been built.
static FOOTER_LEGENDS: MainThreadSlot<Vec<FooterLegend>> = MainThreadSlot::new(Vec::new());

/// The footer's entry-count label. It is not a legend, but it shares the footer's line and owns
/// the left edge the right-to-left legend row must clear, so the validator measures it too.
static FOOTER_COUNT_LABEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

/// The footer root: rebuilt on locale changes so shortcut legends reflow to their new widths.
static FOOTER_VIEW: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);

/// the toast label (the new mockup's .toast).
pub(super) static TOAST_LABEL: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// the toast's auto-hide timer.
static TOAST_TIMER: MainThreadSlot<Option<ObjPtr>> = MainThreadSlot::new(None);
/// the toast timer's target.
unsafe fn toast_owner() -> *mut AnyObject {
    static TOAST_OWNER: OnceLock<CallbackTarget> = OnceLock::new();
    TOAST_OWNER
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipToast").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(dismissToast:),
                toast_dismiss as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(obj)
        })
        .0
}

/// **load-bearing**: a non-repeating timer is released by the run loop after firing, so
/// TOAST_TIMER must be cleared here -- otherwise the next show_toast calls invalidate on
/// a dangling pointer (when the memory now holds some other object, objc2 panics with
/// "method not found"; reproduced by a second ← press to unpin).
extern "C" fn toast_dismiss(_self: *mut c_void, _cmd: Sel, _timer: *mut c_void) {
    *TOAST_TIMER.lock().unwrap() = None;
    unsafe {
        if let Some(label) = *TOAST_LABEL.lock().unwrap() {
            let _: () = msg_send![label.0, setHidden: true];
        }
    }
}

/// Show a toast (the new mockup's .toast): a dark rounded pill at the bottom center,
/// auto-hidden after ~1.4s.
pub(super) fn show_toast(msg: &str) {
    unsafe {
        let label = match *TOAST_LABEL.lock().unwrap() {
            Some(l) => l.0,
            None => return,
        };
        let text = make_nsstring(msg);
        let _: () = msg_send![label, setStringValue: text];
        CFRelease(text as *const c_void);
        // width follows the text, horizontally centered.
        let w = localized_string_width(msg, 11.0) + 24.0;
        let label_frame: NSRect = msg_send![label, frame];
        let x = (PICKER_W - w) / 2.0;
        let _: () = msg_send![label, setFrame: NSRect::new(
            NSPoint::new(x, label_frame.origin.y),
            NSSize::new(w, label_frame.size.height)
        )];
        let _: () = msg_send![label, setHidden: false];
        // Invalidate the previous pending timer and restart; ALWAYS clear the pointer
        // (an invalidated or already-fired timer's pointer is dead -- keeping it would
        // leave a dangling pointer for the next invalidate).
        if let Some(t) = *TOAST_TIMER.lock().unwrap() {
            let _: () = msg_send![t.0, invalidate];
        }
        *TOAST_TIMER.lock().unwrap() = None;
        let timer: *mut AnyObject = msg_send![
            class!(NSTimer),
            scheduledTimerWithTimeInterval: 1.4f64,
            target: toast_owner(),
            selector: sel!(dismissToast:),
            userInfo: std::ptr::null::<AnyObject>(),
            repeats: false
        ];
        *TOAST_TIMER.lock().unwrap() = Some(ObjPtr::new(timer));
    }
}

/// Refresh the footer's entry count (called on every rebuild_rows; the label exists once
/// the window has been built).
pub(super) fn refresh_footer_count(total: usize) {
    unsafe {
        let label = match *FOOTER_COUNT.lock().unwrap() {
            Some(l) => l.0,
            None => return,
        };
        let text = t_count("clipboard.footer_count", total);
        let ns = make_nsstring(&text);
        let _: () = msg_send![label, setStringValue: ns];
        CFRelease(ns as *const c_void);
    }
}

/// Build the footer (the mockup's .footer): a top hairline + the entry count + shortcut
/// legends (kbd keycaps). Non-flipped coords (y=0 at the bottom), 43pt tall, pinned to the
/// window's bottom edge.
pub(super) unsafe fn build_footer(parent: *mut AnyObject, w: f64) {
    // Put the footer in its own root view so locale changes can replace it as a whole instead
    // of retaining pointers to every individual legend.
    let footer: *mut AnyObject = msg_send![class!(NSView), alloc];
    let footer: *mut AnyObject = msg_send![
        footer,
        initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w, FOOTER_H))
    ];
    let _: () = msg_send![parent, addSubview: footer];
    release_obj(footer);
    *FOOTER_VIEW.lock().unwrap() = Some(ObjPtr::new(footer));
    let parent = footer;

    // the top hairline.
    let line: *mut AnyObject = msg_send![class!(NSView), alloc];
    let line: *mut AnyObject = msg_send![
        line,
        initWithFrame: NSRect::new(
            NSPoint::new(0.0, FOOTER_H - 1.0),
            NSSize::new(w, 1.0)
        )
    ];
    let _: () = msg_send![line, setWantsLayer: true];
    let llayer: *mut AnyObject = msg_send![line, layer];
    crate::ffi::layer_set_background(
        llayer,
        crate::ffi::hex_to_cg_color(clipboard_palette().separator),
    );
    let _: () = msg_send![parent, addSubview: line];
    release_obj(line);

    // the entry-count label.
    let count_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
    let count_label: *mut AnyObject = msg_send![
        count_label,
        initWithFrame: NSRect::new(
            NSPoint::new(FOOTER_PAD_X, (FOOTER_H - 14.0) / 2.0),
            NSSize::new(FOOTER_COUNT_W, 14.0)
        )
    ];
    let _: () = msg_send![count_label, setBezeled: false];
    let _: () = msg_send![count_label, setDrawsBackground: false];
    let _: () = msg_send![count_label, setEditable: false];
    let _: () = msg_send![count_label, setSelectable: false];
    let _: () = msg_send![count_label, setUsesSingleLineMode: true];
    let _: () = msg_send![count_label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
    let cf: *mut AnyObject =
        msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
    let _: () = msg_send![count_label, setFont: cf];
    let cc = crate::ffi::hex_to_ns_color(clipboard_palette().muted_text);
    let _: () = msg_send![count_label, setTextColor: cc];
    let _: () = msg_send![parent, addSubview: count_label];
    release_obj(count_label);
    *FOOTER_COUNT.lock().unwrap() = Some(ObjPtr::new(count_label));
    *FOOTER_COUNT_LABEL.lock().unwrap() = Some(ObjPtr::new(count_label));

    // The shortcut legends (kbd keycap + label) are laid out right-to-left on one row.
    let kbd_keys = ["↵", "⌫", "→", "←", "Tab"];
    let kbd_labels = [
        t("clipboard.kbd_paste"),
        t("clipboard.kbd_delete"),
        t("clipboard.kbd_detail"),
        t("clipboard.kbd_pin"),
        t("clipboard.kbd_filter"),
    ];
    let kbd_min_w = 21.0;
    let kbd_h = 19.0;
    // Group gap between a key symbol and its label, reused by the width reservation below.
    const KBD_LABEL_GAP: f64 = 5.0;
    // A legend holds one short symbol ("↵", "⌫", "→", "←") or the word "Tab". Only the word
    // needs more than the minimum: a centered single-line field reserves cell padding on top of
    // the text, so "Tab" needs 28.37pt of cell for 20.37pt of text and renders "T…" at anything
    // less. Measure it rather than hardcoding a width.
    let key_w_for = |key: &str| -> f64 {
        (localized_string_width(key, crate::theme::FONT_CAPTION) + KBD_CELL_PADDING).max(kbd_min_w)
    };
    // Reserve each label's frame from the font it is drawn with. The frame is allocated before
    // the font is assigned, so deriving it from any other size silently under-reserves: the
    // design-system pass moved the drawn font from 10pt to 12pt and left the measurement at
    // 10pt, which wrapped "输入选中条目" down to "输入选中条" and "Tab" to "Ta".
    let mut x = w - FOOTER_PAD_X;
    let mut legends = Vec::with_capacity(kbd_keys.len());
    for (i, key) in kbd_keys.iter().enumerate() {
        let key_w = key_w_for(key);
        let label_w = legend_required_width(&kbd_labels[i]);
        let group_w = key_w + KBD_LABEL_GAP + label_w;
        x -= group_w;
        // The key symbol's chip. These symbols are a *legend*, not controls: they have no
        // target/action, so they must not borrow the bordered control look (design-style §10
        // reserves hover/pressed/focus states, and the border that implies them, for things you
        // can actually operate). The chip keeps only what a legend needs -- a grouping surface
        // that binds the symbol to its label and keeps the row scannable -- hence no border and
        // a fill lighter than `field_bg`.
        let cap: *mut AnyObject = msg_send![class!(NSView), alloc];
        let cap: *mut AnyObject = msg_send![
            cap,
            initWithFrame: NSRect::new(
                NSPoint::new(x, (FOOTER_H - kbd_h) / 2.0),
                NSSize::new(key_w, kbd_h)
            )
        ];
        let _: () = msg_send![cap, setWantsLayer: true];
        let clayer: *mut AnyObject = msg_send![cap, layer];
        crate::ffi::layer_set_background(clayer, legend_chip_background());
        // No border: a border is what makes a chip read as a pressable control.
        let _: () = msg_send![clayer, setBorderWidth: 0.0f64];
        let _: () = msg_send![clayer, setCornerRadius: crate::theme::RADIUS_LEGEND_CHIP];
        // NSTextField top-aligns its glyph, so a full-height label would float the arrow at
        // the chip's top; hug the line height and center it inside the 19pt chip instead.
        let key_label: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let key_label: *mut AnyObject = msg_send![
            key_label,
            initWithFrame: NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(key_w, kbd_h)
            )
        ];
        let _: () = msg_send![key_label, setBezeled: false];
        let _: () = msg_send![key_label, setDrawsBackground: false];
        let _: () = msg_send![key_label, setEditable: false];
        let _: () = msg_send![key_label, setSelectable: false];
        let _: () = msg_send![key_label, setAlignment: 1isize]; // Center on arm64
        let kf: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
        let _: () = msg_send![key_label, setFont: kf];
        // Single-line like the hint: the glyph field is one line high, so the default wrapping
        // would drop the tail invisibly rather than show it was clipped.
        let _: () = msg_send![key_label, setUsesSingleLineMode: true];
        let _: () = msg_send![key_label, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
                                                                    // Hug the font's real line height and center it vertically. A frame of exactly the line
                                                                    // height is one rounding step short of what the cell needs, and a single-line cell that
                                                                    // is even fractionally short renders the tail truncation ("Tab" -> "T..."), so keep the
                                                                    // slack the hint already carries.
        let asc: f64 = msg_send![kf, ascender];
        let desc: f64 = msg_send![kf, descender];
        let line_h = (asc - desc + 1.0).max(11.0) + FOOTER_LABEL_HEIGHT_SLACK;
        let _: () = msg_send![key_label, setFrame: NSRect::new(
            NSPoint::new(0.0, (kbd_h - line_h) / 2.0),
            NSSize::new(key_w, line_h)
        )];
        let kc = crate::ffi::hex_to_ns_color(clipboard_palette().secondary_text);
        let _: () = msg_send![key_label, setTextColor: kc];
        let key_ns = make_nsstring(key);
        let _: () = msg_send![key_label, setStringValue: key_ns];
        CFRelease(key_ns as *const c_void);
        let _: () = msg_send![cap, addSubview: key_label];
        release_obj(key_label);
        let _: () = msg_send![parent, addSubview: cap];
        release_obj(cap);
        // The hint's frame already reserves the text plus FOOTER_LABEL_SLACK; its height uses
        // the font's real line height and is centered. The old fixed 16pt NSTextField drew
        // from its top, making the hint sit slightly above the keycap glyph.
        let hf: *mut AnyObject =
            msg_send![class!(NSFont), systemFontOfSize: crate::theme::FONT_CAPTION];
        let hint_asc: f64 = msg_send![hf, ascender];
        let hint_desc: f64 = msg_send![hf, descender];
        // Same slack as the keycap: a frame of exactly the line height truncates the tail.
        let hint_line_h = (hint_asc - hint_desc + 1.0).max(11.0) + FOOTER_LABEL_HEIGHT_SLACK;
        let hint: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let hint: *mut AnyObject = msg_send![
            hint,
            initWithFrame: NSRect::new(
                NSPoint::new(x + key_w + KBD_LABEL_GAP, (FOOTER_H - hint_line_h) / 2.0),
                NSSize::new(label_w, hint_line_h)
            )
        ];
        let _: () = msg_send![hint, setBezeled: false];
        let _: () = msg_send![hint, setDrawsBackground: false];
        let _: () = msg_send![hint, setEditable: false];
        let _: () = msg_send![hint, setSelectable: false];
        // The frame is one line high, so the default word-wrapping would draw the first line and
        // drop the rest without a trace (Chinese has no word boundaries, making the split
        // arbitrary). Clip at the tail instead: a future regression shows as "删…" rather than
        // as a silently truncated word that reads like a complete label.
        let _: () = msg_send![hint, setUsesSingleLineMode: true];
        let _: () = msg_send![hint, setLineBreakMode: 4isize]; // NSLineBreakByTruncatingTail
        let _: () = msg_send![hint, setFont: hf];
        // One step lighter than the key symbol beside it, but never a fixed color: a literal
        // black was invisible on the dark panel (1.16:1 -- the labels simply vanished) and was
        // already under the 4.5:1 text floor in light mode (2.32:1). `muted_text` is the palette
        // token for subordinate text and clears the floor in both modes (5.0:1 / 4.7:1).
        let hc = crate::ffi::hex_to_ns_color(clipboard_palette().muted_text);
        let _: () = msg_send![hint, setTextColor: hc];
        let hint_ns = make_nsstring(&kbd_labels[i]);
        let _: () = msg_send![hint, setStringValue: hint_ns];
        CFRelease(hint_ns as *const c_void);
        let _: () = msg_send![parent, addSubview: hint];
        release_obj(hint);
        legends.push(FooterLegend {
            cap: ObjPtr::new(cap),
            key_label: ObjPtr::new(key_label),
            hint: ObjPtr::new(hint),
        });
        // spacing before the next group.
        x -= FOOTER_GROUP_GAP;
        let _ = i;
    }
    *FOOTER_LEGENDS.lock().unwrap() = legends;
}

/// Width one shortcut label needs when drawn with the font it is actually assigned, plus the
/// cell slack the footer reserves. Measuring against the drawn font is the point: the frame is
/// allocated before the font is set, so a size that disagrees with the drawn font silently
/// reserves too little and the label wraps out of its one-line-high field.
unsafe fn legend_required_width(label: &str) -> f64 {
    localized_string_width(label, crate::theme::FONT_CAPTION) + FOOTER_LABEL_SLACK
}

/// Whether a label renders completely inside the frame it was given. Two independent ways to
/// lose text, both silent before this check existed:
/// - **width**: it must stay on one line, and it must not wrap at all (a wrapped NSTextField
///   whose frame is one line high draws only the first line and drops the rest);
/// - **height**: a single-line cell whose frame is even fractionally shorter than the line height
///   reports truncation and renders the ellipsis, which is how "Tab" drew as "T...".
unsafe fn label_renders_fully(
    text: *mut AnyObject,
    font: *mut AnyObject,
    width: f64,
    height: f64,
) -> (bool, String) {
    let len: usize = msg_send![text, length];
    let mut buffer = vec![0u16; len];
    if len > 0 {
        let _: () =
            msg_send![text, getCharacters: buffer.as_mut_ptr(), range: NSRange::new(0, len)];
    }
    let content = String::from_utf16_lossy(&buffer);

    let storage: *mut AnyObject = msg_send![class!(NSMutableAttributedString), alloc];
    let storage: *mut AnyObject =
        msg_send![storage, initWithString: text, attributes: std::ptr::null_mut::<AnyObject>()];
    let font_key = make_nsstring("NSFont");
    let _: () =
        msg_send![storage, addAttribute: font_key, value: font, range: NSRange::new(0, len)];
    CFRelease(font_key as *const c_void);
    // Height 1e4: the question is how the text lays out at this width, not how it is clipped.
    // Returns a rect; only its height matters (one line vs. wrapped).
    let laid_out: NSRect = msg_send![
        storage,
        boundingRectWithSize: NSSize::new(width, 1.0e4),
        options: 1usize
    ];
    let (asc, desc): (f64, f64) = {
        let asc: f64 = msg_send![font, ascender];
        let desc: f64 = msg_send![font, descender];
        (asc, desc)
    };
    let line_height = asc - desc + 1.0;
    let stays_on_one_line = laid_out.size.height <= line_height + 0.5;
    // The cell needs the font's real line height; anything less truncates.
    let tall_enough = height + 0.01 >= (asc - desc);
    release_obj(storage);
    (stays_on_one_line && tall_enough, content)
}

/// Validate the footer's shortcut legends: every label must be wide enough for the font it is
/// drawn with, must stay on one line, and must not overlap the neighbouring legend. Reproduces
/// the 2026-10-01 regression where the design-system pass moved the drawn font to
/// FONT_CAPTION (12pt) while the width was still measured at the old 10pt, so "输入选中条目"
/// rendered as "输入选中条" and "Tab" as "Ta". Diagnostics go to stderr: this runner's process
/// never initializes the logger, so a `log_info!` here would be discarded.
pub(super) unsafe fn footer_legends_layout_is_sane() -> bool {
    let legends = match FOOTER_LEGENDS.lock() {
        Ok(legends) => legends,
        Err(_) => return false,
    };
    if legends.is_empty() {
        eprintln!("[smoke-clipboard] footer legends missing");
        return false;
    }
    let count_right_edge = FOOTER_COUNT_LABEL.lock().ok().and_then(|label| {
        label.map(|label| {
            let frame: NSRect = msg_send![label.0, frame];
            frame.origin.x + frame.size.width
        })
    });
    // A legend's text must be legible on the surface it is actually drawn on. The footer's
    // surface is a glass backdrop, so its effective color is not one palette value; measure
    // against a light and a dark representative of it, and check the text colors in **both**
    // resolved modes instead of trusting whichever one this process happens to run in.
    // theme.rs's color helpers work on 0-255 channels, so state these the same way.
    const FOOTER_SURFACE_LIGHT: [f64; 3] = [249.0, 249.0, 251.0];
    const FOOTER_SURFACE_DARK: [f64; 3] = [42.0, 42.0, 44.0];

    let mut ok = true;
    let mut previous_left_edge: Option<f64> = None;
    for (i, legend) in legends.iter().enumerate() {
        let hint_frame: NSRect = msg_send![legend.hint.0, frame];
        let cap_frame: NSRect = msg_send![legend.cap.0, frame];
        let font: *mut AnyObject = msg_send![legend.hint.0, font];
        let text: *mut AnyObject = msg_send![legend.hint.0, stringValue];

        // 3. Every legend text color clears the text contrast floor against the footer surface.
        //    This guards a fixed-color bug: the labels were a literal black at 34% alpha, which
        //    measured 1.16:1 on the dark panel (they vanished outright) and 2.32:1 on the light
        //    one -- already below the 4.5:1 floor for text. Both modes are checked from their own
        //    palette, because this process only ever runs in one of them: checking the live color
        //    against the live surface would silently skip whichever mode is not active here.
        for (dark, surface) in [(false, FOOTER_SURFACE_LIGHT), (true, FOOTER_SURFACE_DARK)] {
            for (role, view) in [("key symbol", legend.key_label.0), ("label", legend.hint.0)] {
                // The token the call site used is what has to be checked, so read it back from
                // the view: a literal introduced there is caught even though it is not a token.
                let _ = (role, view);
                let color: *mut AnyObject = msg_send![view, textColor];
                let Some(fg) = crate::ffi::ns_color_components(color) else {
                    ok = false;
                    eprintln!(
                        "[smoke-clipboard] legend {i} {role} color is not convertible to RGB: \
                         contrast against the footer cannot be verified"
                    );
                    continue;
                };
                let composite = [
                    fg[0] * 255.0 * fg[3] + surface[0] * (1.0 - fg[3]),
                    fg[1] * 255.0 * fg[3] + surface[1] * (1.0 - fg[3]),
                    fg[2] * 255.0 * fg[3] + surface[2] * (1.0 - fg[3]),
                ];
                let contrast = crate::theme::contrast_ratio(composite, surface);
                const MIN_TEXT_CONTRAST: f64 = 4.5;
                // Only the mode this process is running in has a meaningful live color; the
                // other mode is covered by the palette-level test below.
                if dark == clipboard_palette().dark && contrast < MIN_TEXT_CONTRAST {
                    ok = false;
                    eprintln!(
                        "[smoke-clipboard] legend {i} {role} measures {contrast:.2}:1 on the {} \
                         footer (min {MIN_TEXT_CONTRAST}:1): it is not legible there",
                        if dark { "dark" } else { "light" }
                    );
                }
            }
        }

        // These symbols are a legend, not controls: feel free to change how they look, but the
        // two properties below are what distinguish "a legend you read" from "a button you
        // press", and both were lost once already when a mechanical design-system pass applied
        // control-scale tokens to them. Checked against fixed bounds rather than against the
        // theme constants, because comparing a value to the constant that produced it can never
        // fail -- and the regression being guarded is exactly someone raising that constant.
        //
        // 1. No border. A border is the control affordance (design-style §10 reserves hover,
        //    pressed and focus states, and the bordered look that promises them, for things the
        //    user can operate). These have no target/action.
        let cap_layer: *mut AnyObject = msg_send![legend.cap.0, layer];
        let cap_border: f64 = msg_send![cap_layer, borderWidth];
        if cap_border > 0.0 {
            ok = false;
            eprintln!(
                "[smoke-clipboard] legend {i} key chip draws a {cap_border}pt border: these \
                 symbols have no target/action, so they must not wear the bordered control look"
            );
        }
        // 2. The chip radius stays proportional to the chip, so it reads as a softened
        //    rectangle rather than a pill.
        const MAX_LEGEND_CHIP_RADIUS_RATIO: f64 = 0.30;
        let cap_radius: f64 = msg_send![cap_layer, cornerRadius];
        let radius_ratio = cap_radius / cap_frame.size.height;
        if radius_ratio > MAX_LEGEND_CHIP_RADIUS_RATIO {
            ok = false;
            eprintln!(
                "[smoke-clipboard] legend {i} chip radius {cap_radius}pt is {:.0}% of its {}pt \
                 height (max {:.0}%): it reads as a pill",
                radius_ratio * 100.0,
                cap_frame.size.height,
                MAX_LEGEND_CHIP_RADIUS_RATIO * 100.0
            );
        }

        let (fits, content) =
            label_renders_fully(text, font, hint_frame.size.width, hint_frame.size.height);
        // Width alone would not have caught this: the frames were always self-consistent, they
        // were computed from the wrong font. Compare against the drawn font's requirement too.
        let required = legend_required_width(&content);
        let has_room = hint_frame.size.width + 0.5 >= required;
        if !fits || !has_room {
            ok = false;
            eprintln!(
                "[smoke-clipboard] footer legend {i} \"{content}\" clips: frame={}pt required={required}pt one_line={fits}",
                hint_frame.size.width
            );
        }
        if hint_frame.origin.x + 0.5 < cap_frame.origin.x + cap_frame.size.width {
            ok = false;
            eprintln!("[smoke-clipboard] footer legend {i} overlaps its keycap");
        }
        // Legends are laid out right-to-left, so each one must start left of the previous one's
        // keycap; equality would overlap the 16pt group gap.
        if let Some(prev) = previous_left_edge {
            if hint_frame.origin.x + hint_frame.size.width > prev + 0.5 {
                ok = false;
                eprintln!("[smoke-clipboard] footer legend {i} overlaps the next legend");
            }
        }
        if let Some(count_right_edge) = count_right_edge {
            if cap_frame.origin.x + 0.5 < count_right_edge {
                ok = false;
                eprintln!(
                    "[smoke-clipboard] footer legend {i} starts at {}pt, inside the count label's right edge {count_right_edge}pt",
                    cap_frame.origin.x
                );
            }
        }
        previous_left_edge = Some(cap_frame.origin.x);

        // The keycap's own glyph must sit inside the cap for the same reason.
        let cap_text: *mut AnyObject = msg_send![legend.key_label.0, stringValue];
        let cap_font: *mut AnyObject = msg_send![legend.key_label.0, font];
        let cap_label_frame: NSRect = msg_send![legend.key_label.0, frame];
        let (cap_fits, cap_content) = label_renders_fully(
            cap_text,
            cap_font,
            cap_label_frame.size.width,
            cap_label_frame.size.height,
        );
        if !cap_fits {
            ok = false;
            eprintln!("[smoke-clipboard] keycap {i} \"{cap_content}\" does not fit its cap");
        }
    }
    ok
}

/// Refresh localization for an already-created clipboard picker. Menus/settings rebuild or
/// retitle on locale changes, but the picker is a long-lived cached window, so its search hint,
/// filters, clear button, and footer must be updated explicitly.
pub fn refresh_localized_ui() {
    unsafe {
        rebuild_search_hint();
        if let Some(search) = *SEARCH_FIELD.lock().unwrap() {
            let _: () = msg_send![search.0, setNeedsDisplay: true];
        }
        if PICKER_WINDOW.lock().unwrap().is_none() {
            return;
        }

        let labels = localized_filter_labels();
        let pills: Vec<*mut AnyObject> = FILTER_PILLS
            .lock()
            .unwrap()
            .iter()
            .map(|pill| pill.0)
            .collect();
        if pills.len() == labels.len() {
            let filters_y = TOP_PAD_Y + SEARCH_H + SEARCH_GAP_Y;
            let mut x = FILTERS_PAD_X;
            for (pill, label) in pills.iter().zip(labels.iter()) {
                let title = make_nsstring(label);
                let _: () = msg_send![*pill, setTitle: title];
                CFRelease(title as *const c_void);
                let width = localized_string_width(label, 12.0) + 12.0;
                let _: () = msg_send![
                    *pill,
                    setFrame: NSRect::new(
                        NSPoint::new(x, filters_y),
                        NSSize::new(width, FILTERS_H)
                    )
                ];
                x += width + FILTER_GAP;
            }
            update_filter_pill_style(false);

            if let Some(buttons) = *CLEAR_HISTORY_ACTION_BUTTONS.lock().unwrap() {
                let labels = [
                    t("clipboard.clear_confirm_unpinned"),
                    t("clipboard.clear_confirm_all"),
                ];
                let widths: [f64; 2] =
                    std::array::from_fn(|i| localized_string_width(&labels[i], 12.0) + 8.0);
                let total_width = widths.iter().sum::<f64>() + CLEAR_CONFIRM_GAP;
                let mut x = PICKER_W - SEARCH_PAD_X - total_width;
                for (index, button) in buttons.iter().enumerate() {
                    let label = &labels[index];
                    let title = make_nsstring(label);
                    let _: () = msg_send![button.0, setTitle: title];
                    CFRelease(title as *const c_void);
                    let frame = NSRect::new(
                        NSPoint::new(x, filters_y + 8.0),
                        NSSize::new(widths[index], 20.0),
                    );
                    let _: () = msg_send![button.0, setFrame: frame];
                    x += widths[index] + CLEAR_CONFIRM_GAP;
                }
            }
        }

        // English footer legends have different widths; replace the whole footer to rerun its
        // right-to-left layout.
        let old_footer = *FOOTER_VIEW.lock().unwrap();
        if let Some(footer) = old_footer {
            let _: () = msg_send![footer.0, removeFromSuperview];
        }
        *FOOTER_VIEW.lock().unwrap() = None;
        *FOOTER_COUNT.lock().unwrap() = None;
        *FOOTER_COUNT_LABEL.lock().unwrap() = None;
        FOOTER_LEGENDS.lock().unwrap().clear();
        if let Some(parent) = *PICKER_CONTENT_PARENT.lock().unwrap() {
            build_footer(parent.0, PICKER_W);
        }
        rebuild_rows();
    }
}

/// Apply a new filter and rebuild the list. An open detail closes first so it never displays
/// a stale entry outside the filtered result.
pub(super) fn apply_clip_filter(filter: ClipFilter) {
    if detail_visible() {
        hide_detail();
    }
    *CLIP_FILTER.lock().unwrap() = filter;
    update_filter_pill_style(true);
    unsafe { rebuild_rows() };
}

/// Filter-pill click: switch the filter and rebuild the list (an out-of-range selection
/// self-heals in rebuild_rows).
pub(super) extern "C" fn filter_pill_clicked(_self: *mut c_void, _cmd: Sel, sender: *mut c_void) {
    let tag: isize = unsafe { msg_send![sender as *mut AnyObject, tag] };
    let f = match tag {
        0 => ClipFilter::All,
        1 => ClipFilter::Text,
        2 => ClipFilter::Image,
        3 => ClipFilter::Link,
        4 => ClipFilter::Code,
        _ => return,
    };
    apply_clip_filter(f);
}

/// Attach a hover tracking area to a row button (header/body): hovering selects the row
/// (same as the switcher overlay).
pub(super) unsafe fn add_hover_tracking(view: *mut AnyObject) {
    // MouseEnteredAndExited (0x01) | ActiveAlways (0x80), the rect is the view's bounds --
    // exactly the switcher overlay's setup. Two load-bearing points: (1) the picker's host
    // app stays inactive behind the nonactivating panel, and ActiveInActiveApp (0x40)
    // delivers no hover events -- ActiveAlways is required (it was 0x40, so hover never
    // fired); (2) no InVisibleRect -- the visible-rect computation inside the scroll
    // container is unreliable, so an explicit bounds rect is used instead.
    let opts: u64 = 0x01 | 0x80;
    let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let bounds: NSRect = msg_send![view, bounds];
    let ta: *mut AnyObject = msg_send![
        ta,
        initWithRect: bounds,
        options: opts,
        owner: view,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![view, addTrackingArea: ta];
    release_obj(ta);
}

/// Attach an auto-resizing tracking area to the fixed picker content parent and deliver its
/// events to the list container, which resolves whole-row hover. InVisibleRect is reliable
/// here because this parent does not track the scrolling document height.
pub(super) unsafe fn add_picker_hover_tracking(view: *mut AnyObject, owner: *mut AnyObject) {
    // MouseEnteredAndExited | MouseMoved | ActiveAlways | InVisibleRect.
    let opts: u64 = 0x01 | 0x02 | 0x80 | 0x200;
    let ta: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
    let ta: *mut AnyObject = msg_send![
        ta,
        initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
        options: opts,
        owner: owner,
        userInfo: std::ptr::null::<AnyObject>()
    ];
    let _: () = msg_send![view, addTrackingArea: ta];
    release_obj(ta);
}

/// Target for row buttons (responds to handleClipboardRowClick:).
/// A singleton: NSControl's setTarget: is weak (no retain), so creating a new instance per
/// rebuild would leak forever; one instance per process lives until exit, and the buttons'
/// weak reference to it stays valid.
pub(super) unsafe fn row_target() -> *mut AnyObject {
    static ROW_TARGET: OnceLock<CallbackTarget> = OnceLock::new();
    ROW_TARGET
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipboardRowTarget").unwrap();
            let superclass = class!(NSObject) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            let types = CString::new("v@:@").unwrap();
            class_addMethod(
                cls,
                sel!(handleClipboardRowClick:),
                handle_clipboard_row_click as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(togglePin:),
                toggle_pin as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(deleteEntry:),
                delete_entry_cb as *mut c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel!(showItemDetails:),
                show_item_details_cb as *mut c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
            // Instance alloc (+1): process-level singleton, never released (matches the
            // static's lifetime).
            let obj: *mut AnyObject = msg_send![cls as *const AnyObject, new];
            CallbackTarget::new(obj)
        })
        .0
}
