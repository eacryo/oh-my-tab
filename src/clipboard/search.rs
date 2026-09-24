//! Clipboard picker search-field cell drawing and layout.
//!
//! The search field owns its custom cell rendering, while event handling remains in the picker
//! controller. Keeping the drawing class here reduces the controller's UI surface area.

use super::*;

/// The search field cell class (an NSSearchFieldCell subclass overriding
/// drawInteriorWithFrame:inView:), which centers the icon and placeholder as a group.
pub(super) unsafe fn search_cell_class() -> *mut AnyObject {
    static CELL_CLS: OnceLock<StaticClass> = OnceLock::new();
    CELL_CLS
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipSearchCell").unwrap();
            let superclass = class!(NSSearchFieldCell) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            // Args: NSRect (struct) + NSView* -> encoding "v@:{CGRect=dddd}@".
            let types = CString::new("v@:{CGRect=dddd}@").unwrap();
            class_addMethod(
                cls,
                sel!(drawInteriorWithFrame:inView:),
                search_cell_draw_interior as *mut c_void,
                types.as_ptr(),
            );
            // Outer-draw override: when focused-and-empty, the search icon and the
            // placeholder are drawn by the superclass's drawWithFrame: (drawInterior only
            // covers the interior text area, verified); skip the whole frame here.
            class_addMethod(
                cls,
                sel!(drawWithFrame:inView:),
                search_cell_draw_with_frame as *mut c_void,
                types.as_ptr(),
            );
            // The field editor is positioned directly at edit start: drawingRectForBounds:
            // has no effect on it (the native NSSearchFieldCell returns the full-height
            // rect, so the override condition never fires and the text stays top-aligned --
            // measured 0.5pt from the field top). selectWithFrame: re-centers the editor
            // frame vertically, centering the caret and the typed text with the line box.
            let types_sel = CString::new("v@:{CGRect=dddd}@@@@qq").unwrap();
            class_addMethod(
                cls,
                sel!(selectWithFrame:inView:editor:delegate:start:length:),
                search_cell_select_with_frame as *mut c_void,
                types_sel.as_ptr(),
            );
            objc_registerClassPair(cls);
            StaticClass(cls as *const objc2::runtime::AnyClass)
        })
        .0 as *mut AnyObject
}

/// Whether the current search needs its right-side clear ×.
pub(super) fn search_has_query() -> bool {
    with_clipboard_ui(|ui| !ui.search_query.is_empty())
}

/// Draws the mockup's ⌘F keycap; it hides once the user types, replaced by the right-side ×.
unsafe fn draw_search_keycap(cell_frame: NSRect) {
    if search_has_query() {
        return;
    }
    let chip_w = 27.0;
    let chip_h = 21.0;
    let chip_rect = NSRect::new(
        NSPoint::new(
            cell_frame.origin.x + cell_frame.size.width - chip_w - SEARCH_PAD_IN,
            cell_frame.origin.y + (cell_frame.size.height - chip_h) / 2.0,
        ),
        NSSize::new(chip_w, chip_h),
    );
    let path: *mut AnyObject = msg_send![
        class!(NSBezierPath),
        bezierPathWithRoundedRect: chip_rect,
        xRadius: 5.0,
        yRadius: 5.0
    ];
    let cap_bg = crate::ffi::hex_to_ns_color(clipboard_palette().field_bg);
    let _: () = msg_send![cap_bg, set];
    let _: () = msg_send![path, fill];
    let chip_font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 10.0f64];
    let chip_attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let chip_attrs: *mut AnyObject = msg_send![chip_attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let _: () = msg_send![chip_attrs, setObject: chip_font, forKey: font_key];
    let chip_color = crate::ffi::hex_to_ns_color(clipboard_palette().secondary_text);
    let _: () = msg_send![chip_attrs, setObject: chip_color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let chip_ns = make_nsstring("⌘F");
    let chip: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let chip: *mut AnyObject = msg_send![chip, initWithString: chip_ns, attributes: chip_attrs];
    CFRelease(chip_ns as *const c_void);
    release_obj(chip_attrs);
    let size: NSSize = msg_send![chip, size];
    let x = chip_rect.origin.x + (chip_rect.size.width - size.width) / 2.0;
    let y = chip_rect.origin.y + (chip_rect.size.height - size.height) / 2.0;
    let _: () = msg_send![chip, drawAtPoint: NSPoint::new(x, y)];
    release_obj(chip);
}

/// Draws the only deliberate HTML deviation: a right-side clear × when text exists.
unsafe fn draw_search_clear(cell_frame: NSRect) {
    if !search_has_query() {
        return;
    }
    let hovered = SEARCH_CLEAR_HOVERED.load(Ordering::SeqCst);
    let clear_x = cell_frame.origin.x + cell_frame.size.width - SEARCH_PAD_IN - SEARCH_CLEAR_W;
    let clear_rect = NSRect::new(
        NSPoint::new(
            clear_x,
            cell_frame.origin.y + (cell_frame.size.height - ACTION_H) / 2.0,
        ),
        NSSize::new(SEARCH_CLEAR_W, ACTION_H),
    );
    if hovered {
        // Match the row delete button: 210/45/40 red glyph with a 7% red rounded fill.
        let path: *mut AnyObject = msg_send![
            class!(NSBezierPath),
            bezierPathWithRoundedRect: clear_rect,
            xRadius: 5.0,
            yRadius: 5.0
        ];
        let bg: *mut AnyObject = msg_send![
            class!(NSColor),
            colorWithSRGBRed: 210.0f64 / 255.0,
            green: 45.0f64 / 255.0,
            blue: 40.0f64 / 255.0,
            alpha: 0.07f64
        ];
        let _: () = msg_send![bg, set];
        let _: () = msg_send![path, fill];
    }
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: 16.0f64];
    let color: *mut AnyObject = if hovered {
        msg_send![
            class!(NSColor),
            colorWithSRGBRed: 210.0f64 / 255.0,
            green: 45.0f64 / 255.0,
            blue: 40.0f64 / 255.0,
            alpha: 0.85f64
        ]
    } else {
        msg_send![class!(NSColor), colorWithWhite: 0.0f64, alpha: 0.42f64]
    };
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let text = make_nsstring("×");
    let cross: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let cross: *mut AnyObject = msg_send![cross, initWithString: text, attributes: attrs];
    CFRelease(text as *const c_void);
    release_obj(attrs);
    let size: NSSize = msg_send![cross, size];
    let x = clear_rect.origin.x + (clear_rect.size.width - size.width) / 2.0;
    let y = clear_rect.origin.y + (clear_rect.size.height - size.height) / 2.0;
    let _: () = msg_send![cross, drawAtPoint: NSPoint::new(x, y)];
    release_obj(cross);
}

/// Draws the consistent hand-crafted "⌕" icon column. The icon itself is centered independently
/// within the 40pt field rather than sharing the hint text's attributed-string baseline; returns
/// the fixed 22pt column width for aligning query/placeholder text.
unsafe fn draw_search_icon_prefix(cell_frame: NSRect) -> Option<NSSize> {
    let hint = (*SEARCH_HINT_TEXT.lock().unwrap())?;
    let icon: *mut AnyObject = msg_send![
        hint.0,
        attributedSubstringFromRange: NSRange::new(0, 1)
    ];
    if icon.is_null() {
        return None;
    }
    let icon_size: NSSize = msg_send![icon, size];
    let x = cell_frame.origin.x + SEARCH_PAD_IN;
    let y = cell_frame.origin.y + (cell_frame.size.height - icon_size.height) / 2.0;
    let _: () = msg_send![icon, drawAtPoint: NSPoint::new(x, y)];
    Some(NSSize::new(SEARCH_ICON_W, icon_size.height))
}

/// Draws the placeholder text independently centered within the 40pt field like the icon and
/// ⌘F keycap, starting after the fixed 22pt icon column.
unsafe fn draw_search_placeholder(cell_frame: NSRect) {
    let Some(hint) = *SEARCH_HINT_TEXT.lock().unwrap() else {
        return;
    };
    let len: usize = msg_send![hint.0, length];
    // "⌕  " occupies the first three UTF-16 units; the remainder retains its existing
    // 14pt/40%-black attributes.
    if len <= 3 {
        return;
    }
    let text: *mut AnyObject = msg_send![
        hint.0,
        attributedSubstringFromRange: NSRange::new(3, len - 3)
    ];
    if text.is_null() {
        return;
    }
    let x = cell_frame.origin.x + SEARCH_PAD_IN + SEARCH_ICON_W;
    // Same rendering as the retained query (the shared NSLayoutManager path + floored
    // position), so the placeholder, the typed text, and the unfocused query are
    // pixel-identical (no jump on the first keystroke or ↓).
    draw_search_query_layout(cell_frame, text, x);
}

/// When a query remains after focus leaves, draw it with the hand-drawn hint's typography and
/// baseline, avoiding a jump from NSTextView's centered line box to NSSearchFieldCell's
/// default higher baseline as ↓ enters the results list.
unsafe fn draw_retained_search_query(cell_frame: NSRect, query: *mut AnyObject) {
    let Some(prefix_size) = draw_search_icon_prefix(cell_frame) else {
        return;
    };
    draw_search_keycap(cell_frame);
    draw_search_clear(cell_frame);
    let attrs: *mut AnyObject = msg_send![class!(NSMutableDictionary), alloc];
    let attrs: *mut AnyObject = msg_send![attrs, init];
    let font_key = make_nsstring("NSFont");
    let color_key = make_nsstring("NSColor");
    let font: *mut AnyObject = msg_send![class!(NSFont), systemFontOfSize: SEARCH_FONT_SIZE];
    let color = crate::ffi::hex_to_ns_color(clipboard_palette().primary_text);
    let _: () = msg_send![attrs, setObject: font, forKey: font_key];
    let _: () = msg_send![attrs, setObject: color, forKey: color_key];
    CFRelease(font_key as *const c_void);
    CFRelease(color_key as *const c_void);
    let value: *mut AnyObject = msg_send![class!(NSAttributedString), alloc];
    let value: *mut AnyObject = msg_send![value, initWithString: query, attributes: attrs];
    release_obj(attrs);
    let x = cell_frame.origin.x + SEARCH_PAD_IN + prefix_size.width;
    draw_search_query_layout(cell_frame, value, x);
    release_obj(value);
}

/// Draws the query/placeholder text through the same NSLayoutManager path as the field
/// editor: drawAtPoint:'s glyph layout differs ~0.5pt from the layout manager (the 'l'
/// stem tip gets clipped), making the text look lower after ↓. A throwaway storage /
/// layoutManager / textContainer makes the rendering pixel-identical to the typed text.
/// The line-box top is floored: the field editor's textContainerOrigin floors the inset
/// to integer points (measured 11.5 -> 11.0); the retained draw must use that same
/// floored position or it ends up 0.5pt off after ↓.
unsafe fn draw_search_query_layout(cell_frame: NSRect, value: *mut AnyObject, x: f64) {
    let font_key = make_nsstring("NSFont");
    let font: *mut AnyObject = msg_send![
        value,
        attribute: font_key,
        atIndex: 0usize,
        effectiveRange: std::ptr::null_mut::<NSRange>()
    ];
    CFRelease(font_key as *const c_void);
    let line_h: f64 = msg_send![font, lineHeight];
    let y_top = ((cell_frame.size.height - line_h) / 2.0).floor();
    let storage: *mut AnyObject = msg_send![class!(NSTextStorage), alloc];
    let storage: *mut AnyObject = msg_send![storage, initWithAttributedString: value];
    let lm: *mut AnyObject = msg_send![class!(NSLayoutManager), alloc];
    let lm: *mut AnyObject = msg_send![lm, init];
    let _: () = msg_send![storage, addLayoutManager: lm];
    let tc: *mut AnyObject = msg_send![class!(NSTextContainer), alloc];
    let tc: *mut AnyObject =
        msg_send![tc, initWithContainerSize: NSSize::new(cell_frame.size.width, line_h)];
    let _: () = msg_send![lm, addTextContainer: tc];
    // Match the editor's container: no line-fragment padding, use font leading.
    let _: () = msg_send![tc, setLineFragmentPadding: 0.0];
    let _: () = msg_send![lm, setUsesFontLeading: true];
    let _: () = msg_send![lm, ensureLayoutForTextContainer: tc];
    let glyph_range: NSRange = msg_send![lm, glyphRangeForTextContainer: tc];
    // The search field is a flipped view (isFlipped=true, measured), so the cell's drawing
    // context and the layout-manager glyphs share the flipped coordinate system: drawing
    // the glyphs directly with the line-box top y_top as the origin matches the field
    // editor exactly. The earlier flipped-offscreen-image composite flipped again on the
    // real (flipped) screen, rendering the text upside down.
    let _: () = msg_send![
        lm,
        drawGlyphsForGlyphRange: glyph_range,
        atPoint: NSPoint::new(x, y_top)
    ];
    release_obj(tc);
    release_obj(lm);
    release_obj(storage);
}

/// Draws the centered placeholder for a non-editing empty field; a retained unfocused query is
/// hand-drawn on the same baseline, while an actively edited value belongs to the field editor.
extern "C" fn search_cell_draw_interior(
    _self: *mut c_void,
    _cmd: Sel,
    cell_frame: NSRect,
    control_view: *mut c_void,
) {
    unsafe {
        // Editing detection: a live field editor means editing (no placeholder; super).
        let editing = if control_view.is_null() {
            false
        } else {
            let editor: *mut AnyObject = msg_send![control_view as *mut AnyObject, currentEditor];
            !editor.is_null()
        };
        let str_obj: *mut AnyObject = msg_send![_self as *mut AnyObject, stringValue];
        let str_len: usize = msg_send![str_obj, length];
        // An unfocused field may still retain a query after ↓ moves focus into the list. Draw
        // the placeholder only while it is empty; otherwise super must draw the entered query.
        if !editing && str_len > 0 {
            draw_retained_search_query(cell_frame, str_obj);
            return;
        }
        if editing && str_len > 0 {
            // The field editor draws text; hand-draw the icon so NSSearchField's stock glyph
            // never replaces the initial ⌕ after typing.
            let _ = draw_search_icon_prefix(cell_frame);
            draw_search_keycap(cell_frame);
            draw_search_clear(cell_frame);
            return;
        }
        if !editing && str_len == 0 {
            // Center the three independent flex items (⌕, hint, ⌘F) by their own sizes in the
            // 40pt field.
            if draw_search_icon_prefix(cell_frame).is_some() {
                draw_search_placeholder(cell_frame);
                draw_search_keycap(cell_frame);
                return;
            }
        }
        // An empty editing string is fully drawn by outer drawWithFrame:, so do not draw it
        // here again. With text (including a query retained after ↓ gives up focus), super draws
        // the icon and field value.
        if str_len == 0 {
            return;
        }
        // Raw objc_msgSendSuper: objc2's msg_send! infinitely recurses in signature
        // verification for the nested-struct encoding (observed stack overflow); CG
        // structs must go through raw FFI (same as ffi::layer_set_*).
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, NSRect, *mut c_void) -> ();
        let super_class =
            objc2::runtime::AnyClass::get(c"NSSearchFieldCell").unwrap() as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class,
        };
        let f: F = std::mem::transmute(objc_msgSendSuper as *const ());
        f(
            &mut sup,
            sel!(drawInteriorWithFrame:inView:),
            cell_frame,
            control_view,
        );
    }
}

/// Outer drawing: with an empty editing string, do not call super (which would paint its
/// left-aligned system placeholder); draw our complete search hint instead. This keeps the
/// HTML mockup's hint visible after a click until the user enters the first character.
extern "C" fn search_cell_draw_with_frame(
    _self: *mut c_void,
    _cmd: Sel,
    cell_frame: NSRect,
    control_view: *mut c_void,
) {
    unsafe {
        // Editing detection (same as drawInterior).
        let editing = if control_view.is_null() {
            false
        } else {
            let editor: *mut AnyObject = msg_send![control_view as *mut AnyObject, currentEditor];
            !editor.is_null()
        };
        if editing {
            let str_obj: *mut AnyObject = msg_send![_self as *mut AnyObject, stringValue];
            let str_len: usize = msg_send![str_obj, length];
            if str_len == 0 {
                // Focused with an empty stringValue -> draw ONLY the magnifier and the
                // ⌘F keycap, never the placeholder text. During IME composition the
                // pinyin lives in the field editor as marked text (stringValue stays
                // empty), so this branch is active too -- drawing the placeholder here
                // would put the pre-edit string right on top of "Search clipboard".
                // The hint hides on focus (native search-field behavior); the keycap
                // hugs the right edge away from the caret and stays.
                let _ = draw_search_icon_prefix(cell_frame);
                draw_search_keycap(cell_frame);
                return;
            }
        }
        // Raw objc_msgSendSuper (same as drawInterior; the struct encoding goes through
        // raw FFI).
        type F = unsafe extern "C" fn(*mut ObjcSuper, Sel, NSRect, *mut c_void) -> ();
        let super_class =
            objc2::runtime::AnyClass::get(c"NSSearchFieldCell").unwrap() as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class,
        };
        let f: F = std::mem::transmute(objc_msgSendSuper as *const ());
        f(
            &mut sup,
            sel!(drawWithFrame:inView:),
            cell_frame,
            control_view,
        );
    }
}

/// Edit-start positioning override: calls the superclass first (icon inset / selection
/// range), then re-centers the editor frame in the cell (line height from font metrics).
/// The editor frame's top is the text container's top, so centering it centers the caret
/// and the typed text.
extern "C" fn search_cell_select_with_frame(
    _self: *mut c_void,
    _cmd: Sel,
    rect: NSRect,
    control_view: *mut c_void,
    editor: *mut c_void,
    delegate: *mut c_void,
    sel_start: isize,
    sel_length: isize,
) {
    unsafe {
        // Raw objc_msgSendSuper (same as drawWithFrame; the struct encoding goes through
        // raw FFI).
        type F = unsafe extern "C" fn(
            *mut ObjcSuper,
            Sel,
            NSRect,
            *mut c_void,
            *mut c_void,
            *mut c_void,
            isize,
            isize,
        ) -> ();
        let super_class =
            objc2::runtime::AnyClass::get(c"NSSearchFieldCell").unwrap() as *const _ as *mut c_void;
        let mut sup = ObjcSuper {
            receiver: _self,
            super_class,
        };
        let f: F = std::mem::transmute(objc_msgSendSuper as *const ());
        f(
            &mut sup,
            sel!(selectWithFrame:inView:editor:delegate:start:length:),
            rect,
            control_view,
            editor,
            delegate,
            sel_start,
            sel_length,
        );
        // The system resets the editor frame during later layout (so setFrame is ineffective),
        // whereas textContainerInset persists. Typing must start after the 12pt field padding
        // plus 22pt icon column, exactly matching draw_retained_search_query after ↓ moves
        // focus to the list, so the text never shifts horizontally.
        if !editor.is_null() && rect.size.height > 0.0 && !control_view.is_null() {
            let font: *mut AnyObject = msg_send![_self as *mut AnyObject, font];
            if !font.is_null() {
                // Force the live field editor to the same font as the hand-drawn unfocused
                // query. Setting NSSearchField alone does not always override a reused shared
                // field editor's default font.
                let _: () = msg_send![editor as *mut AnyObject, setFont: font];
                // The caret/line-fragment height is the font's lineHeight (measured 17.0),
                // not asc-desc+lead (16.49): centering by the latter leaves the 17pt-tall
                // fragment/caret center at 20.25, 0.25pt off the field's center.
                let line_h: f64 = msg_send![font, lineHeight];
                if line_h > 0.0 && line_h < rect.size.height {
                    // Vertical centering must be computed against the editor's TOP in the
                    // field's coordinate system: the typed text's top edge = the editor's
                    // top offset above the field + the inset, and the line box's center must
                    // land on the field's vertical center. The old code ADDED the frame's
                    // height excess to the inset, which inflated the shared field editor by
                    // 2/4/8/16pt on every focus (a positive feedback loop -- the bigger the
                    // inset, the taller it grew), drifting the text lower each cycle. Now the
                    // editor's top offset (converted into field coordinates) is SUBTRACTED,
                    // so the inset self-corrects against the editor's real position: the text
                    // stays centered and the editor stops growing.
                    let ed_bounds: NSRect = msg_send![editor as *mut AnyObject, bounds];
                    let in_field: NSRect = msg_send![
                        control_view as *mut AnyObject,
                        convertRect: ed_bounds,
                        fromView: editor as *mut AnyObject
                    ];
                    // How far the editor's top sits above the field's top (non-flipped:
                    // the top edge is maxY).
                    let top_offset = in_field.origin.y + in_field.size.height - rect.size.height;
                    let vertical = ((rect.size.height - line_h) / 2.0 - top_offset).max(0.0);
                    let _: () = msg_send![
                        editor as *mut AnyObject,
                        setTextContainerInset: NSSize::new(SEARCH_PAD_IN + SEARCH_ICON_W, vertical)
                    ];
                    // NSTextContainer's default lineFragmentPadding = 2 pushes the typed text
                    // 2pt right of the hand-drawn retained query's start (12 + 22 = 34),
                    // making the text jump left on ↓. Zeroing it makes the glyphs start at
                    // the container origin (the inset start), matching the hand-drawn x.
                    let tc: *mut AnyObject = msg_send![editor as *mut AnyObject, textContainer];
                    let _: () = msg_send![tc, setLineFragmentPadding: 0.0];
                }
            }
        }
    }
}
