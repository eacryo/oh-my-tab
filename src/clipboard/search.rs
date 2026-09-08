//! Clipboard picker search-field cell drawing and layout.
//!
//! The search field owns its custom cell rendering, while event handling remains in the picker
//! controller. Keeping the drawing class here reduces the controller's UI surface area.
//!
//! 剪贴板选择器搜索框单元格绘制与布局。
//!
//! 搜索框负责自定义单元格绘制，事件处理仍由选择器控制器负责；将绘制类独立出来以缩小控制器范围。

use super::*;

/// 搜索框 cell 类(NSSearchFieldCell 子类,覆写 drawInteriorWithFrame:inView:)。
/// 非编辑态且有占位文字时,自绘"放大镜图标 + 占位文字"整体水平居中。
/// The search field cell class (an NSSearchFieldCell subclass overriding
/// drawInteriorWithFrame:inView:), which centers the icon and placeholder as a group.
pub(super) unsafe fn search_cell_class() -> *mut AnyObject {
    static CELL_CLS: OnceLock<StaticClass> = OnceLock::new();
    CELL_CLS
        .get_or_init(|| {
            let name = CString::new("OhMyTabClipSearchCell").unwrap();
            let superclass = class!(NSSearchFieldCell) as *const _ as *mut AnyObject;
            let cls = objc_allocateClassPair(superclass, name.as_ptr(), 0);
            // 参数:NSRect(struct) + NSView* -> 编码 "v@:{CGRect=dddd}@"。
            // Args: NSRect (struct) + NSView* -> encoding "v@:{CGRect=dddd}@".
            // super 调用走原始 objc_msgSendSuper(见 search_cell_draw_super):objc2 的
            // msg_send! 对新式嵌套结构编码 {CGRect={CGPoint=dd}{CGSize=dd}} 会在签名
            // 校验里无限递归(实测栈溢出),CG 结构必须走 raw FFI(与 layer_set_* 同款)。
            let types = CString::new("v@:{CGRect=dddd}@").unwrap();
            class_addMethod(
                cls,
                sel!(drawInteriorWithFrame:inView:),
                search_cell_draw_interior as *mut c_void,
                types.as_ptr(),
            );
            // 外层绘制覆写:聚焦空字段时搜索图标与占位都由父类的 drawWithFrame: 画出
            // (drawInterior 只画内部文字区,实测管不到),在这里整帧跳过。
            // Outer-draw override: when focused-and-empty, the search icon and the
            // placeholder are drawn by the superclass's drawWithFrame: (drawInterior only
            // covers the interior text area, verified); skip the whole frame here.
            class_addMethod(
                cls,
                sel!(drawWithFrame:inView:),
                search_cell_draw_with_frame as *mut c_void,
                types.as_ptr(),
            );
            // 编辑启动时直接定位字段编辑器:drawingRectForBounds: 对编辑器无效
            // (原生 NSSearchFieldCell 返回整框高度,覆写条件不会成立,文本始终贴顶,
            // 实测光标顶端与字段上缘仅差 0.5pt)。在 selectWithFrame: 里把编辑器
            // frame 垂直居中,光标与输入文字随行框一起居中。
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

/// 当前搜索是否需要显示右侧清除叉号。
/// Whether the current search needs its right-side clear ×.
pub(super) fn search_has_query() -> bool {
    with_clipboard_ui(|ui| !ui.search_query.is_empty())
}

/// 绘制设计稿的 ⌘F 键帽;用户开始输入后隐藏,由右侧清除叉号取代。
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

/// 绘制唯一偏离 HTML 的右侧清除叉号(仅有输入时出现)。
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
        // 与行内删除按钮相同:210/45/40 红色文字 + 7% 红色圆角底。
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

/// 手绘始终一致的 "⌕" 图标列。图标本身在 40pt 高度内独立居中,而不是和提示
/// 文本共用富文本基线;返回固定 22pt 列宽供查询/占位文字对齐。
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

/// 手绘提示文本:与图标、⌘F 键帽一样在 40pt 高度内独立垂直居中,并从固定 22pt
/// 图标列之后开始。
/// Draws the placeholder text independently centered within the 40pt field like the icon and
/// ⌘F keycap, starting after the fixed 22pt icon column.
unsafe fn draw_search_placeholder(cell_frame: NSRect) {
    let Some(hint) = *SEARCH_HINT_TEXT.lock().unwrap() else {
        return;
    };
    let len: usize = msg_send![hint.0, length];
    // "⌕  " 占前三个 UTF-16 单元;余下部分保留已有的 14pt/40% 黑属性。
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
    // 与保留查询同款(同一 NSLayoutManager 路径 + floor 定位),保证占位/输入/失焦三种
    // 状态文字逐像素一致,首字符键入与 ↓ 切换都不跳动。
    // Same rendering as the retained query (the shared NSLayoutManager path + floored
    // position), so the placeholder, the typed text, and the unfocused query are
    // pixel-identical (no jump on the first keystroke or ↓).
    draw_search_query_layout(cell_frame, text, x);
}

/// 失焦但仍有查询时,用与手绘占位相同的字体和基线绘制值,避免 ↓ 进入结果列表时从
/// NSTextView 的居中行框跳到 NSSearchFieldCell 的默认偏上基线。
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

/// 用字段编辑器同款 NSLayoutManager 路径绘制查询/占位文本:drawAtPoint: 的字形布局与
/// layout manager 有 ~0.5pt 差异('l' 字干顶端会被截短),按 ↓ 切换时文字看似下移。
/// 建临时 storage/layoutManager/textContainer,画完即弃,保证与输入态文字逐像素一致。
/// Draws the query/placeholder text through the same NSLayoutManager path as the field
/// editor: drawAtPoint:'s glyph layout differs ~0.5pt from the layout manager (the 'l'
/// stem tip gets clipped), making the text look lower after ↓. A throwaway storage /
/// layoutManager / textContainer makes the rendering pixel-identical to the typed text.
/// 行框顶边取 floor:字段编辑器的 textContainerOrigin 会把 inset 向下取整到整数点
/// (实测 11.5 → 11.0),保留绘制必须用同一个取整后的位置,否则按 ↓ 会差 0.5pt。
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
    // 与编辑器容器同款配置:无行片段内边距、使用字体 leading。
    // Match the editor's container: no line-fragment padding, use font leading.
    let _: () = msg_send![tc, setLineFragmentPadding: 0.0];
    let _: () = msg_send![lm, setUsesFontLeading: true];
    let _: () = msg_send![lm, ensureLayoutForTextContainer: tc];
    let glyph_range: NSRange = msg_send![lm, glyphRangeForTextContainer: tc];
    // 搜索框是翻转视图(isFlipped=true,实测),cell 绘制上下文与 LM 字形同为翻转坐标:
    // 直接以行框顶边 y_top 为原点画字形即可,与字段编辑器完全一致。之前用翻转离屏图
    // 合成,NSImage 的翻转语义随目标上下文变化——真实屏幕(翻转)上会上下颠倒。
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

/// 居中自绘占位:非编辑态 + 空字段 → 把"放大镜 + 文字"整体画在 cell 水平中心;
/// 失焦的已输入查询走同基线自绘,编辑态交给字段编辑器。
/// Draws the centered placeholder for a non-editing empty field; a retained unfocused query is
/// hand-drawn on the same baseline, while an actively edited value belongs to the field editor.
extern "C" fn search_cell_draw_interior(
    _self: *mut c_void,
    _cmd: Sel,
    cell_frame: NSRect,
    control_view: *mut c_void,
) {
    unsafe {
        // 编辑态检测:字段编辑器存在 = 正在编辑(占位不显示,交给父类)。
        // Editing detection: a live field editor means editing (no placeholder; super).
        let editing = if control_view.is_null() {
            false
        } else {
            let editor: *mut AnyObject = msg_send![control_view as *mut AnyObject, currentEditor];
            !editor.is_null()
        };
        let str_obj: *mut AnyObject = msg_send![_self as *mut AnyObject, stringValue];
        let str_len: usize = msg_send![str_obj, length];
        // 未聚焦时也可能保留搜索词(↓ 将焦点交给列表)。仅空字段可绘制占位;
        // 否则必须让父类绘制已输入的查询。
        // An unfocused field may still retain a query after ↓ moves focus into the list. Draw
        // the placeholder only while it is empty; otherwise super must draw the entered query.
        if !editing && str_len > 0 {
            draw_retained_search_query(cell_frame, str_obj);
            return;
        }
        if editing && str_len > 0 {
            // 文字由字段编辑器绘制;图标必须手绘,避免 NSSearchField 的 stock glyph 在
            // 输入后替换初始的 ⌕。
            // The field editor draws text; hand-draw the icon so NSSearchField's stock glyph
            // never replaces the initial ⌕ after typing.
            let _ = draw_search_icon_prefix(cell_frame);
            draw_search_keycap(cell_frame);
            draw_search_clear(cell_frame);
            return;
        }
        if !editing && str_len == 0 {
            // 三个独立 flex 项(⌕、提示、⌘F)均按自己的尺寸在 40pt 高度内居中。
            // Center the three independent flex items (⌕, hint, ⌘F) by their own sizes in the
            // 40pt field.
            if draw_search_icon_prefix(cell_frame).is_some() {
                draw_search_placeholder(cell_frame);
                draw_search_keycap(cell_frame);
                return;
            }
        }
        // 编辑态空字符串由外层 drawWithFrame: 自绘完整提示,这里不重复绘制。有文字
        // (包括 ↓ 交出焦点后保留的查询)→ 父类画图标和字段值。
        // An empty editing string is fully drawn by outer drawWithFrame:, so do not draw it
        // here again. With text (including a query retained after ↓ gives up focus), super draws
        // the icon and field value.
        if str_len == 0 {
            return;
        }
        // 原始 objc_msgSendSuper:objc2 的 msg_send! 对嵌套结构编码会在签名校验里
        // 无限递归(实测栈溢出),CG 结构必须走 raw FFI(与 ffi::layer_set_* 同款)。
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

/// 外层绘制:编辑态空字符串时不调用父类(否则会画出左对齐的系统占位),改走自绘完整
/// 搜索提示。这样 HTML 原本的提示在点击后仍可见,直到用户输入首个字符。
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
        // 编辑态检测(与 drawInterior 同款)。
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
                // 聚焦且 stringValue 为空 → 只画放大镜 + ⌘F 键帽,**不画占位文字**。
                // IME 组合期间拼音是字段编辑器里的 marked text,不进 stringValue,
                // 此分支同样命中——若在这里画占位,编辑器的预编辑串会叠在
                // "Search clipboard" 上。聚焦即隐藏提示(与原生搜索框一致);
                // ⌘F 键帽贴右缘,不在拼音落点区域,保留。
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
        // 原始 objc_msgSendSuper(与 drawInterior 同款,结构编码走 raw FFI)。
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

/// 编辑启动定位覆写:先走父类(图标留白/选择范围等),再把编辑器 frame 调整为
/// cell 内垂直居中(行框高度来自字体度量)。编辑器 frame 的顶部即文字容器顶部,
/// 居中后光标与输入文字随行框对称分布。
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
        // 原始 objc_msgSendSuper(与 drawWithFrame 同款,结构编码走 raw FFI)。
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
        // 编辑器 frame 会被系统在后续布局中重置(setFrame 无效),而 textContainerInset
        // 是持久属性。输入态必须从 12pt 内边距 + 22pt 图标列开始,与 ↓ 交出焦点后
        // draw_retained_search_query 的坐标完全一致,避免文本横向跳动。
        // The system resets the editor frame during later layout (so setFrame is ineffective),
        // whereas textContainerInset persists. Typing must start after the 12pt field padding
        // plus 22pt icon column, exactly matching draw_retained_search_query after ↓ moves
        // focus to the list, so the text never shifts horizontally.
        if !editor.is_null() && rect.size.height > 0.0 && !control_view.is_null() {
            let font: *mut AnyObject = msg_send![_self as *mut AnyObject, font];
            if !font.is_null() {
                // 强制 live field editor 使用与失焦手绘查询相同的字体。仅设置
                // NSSearchField 并不总会覆盖复用的 shared field editor 的默认字体。
                // Force the live field editor to the same font as the hand-drawn unfocused
                // query. Setting NSSearchField alone does not always override a reused shared
                // field editor's default font.
                let _: () = msg_send![editor as *mut AnyObject, setFont: font];
                // 光标/行片段高度 = 字体的 lineHeight(实测 17.0),而非 asc-desc+lead
                // (16.49):按后者居中会让 17pt 高的片段/光标中心落在 20.25,偏高 0.25pt。
                // The caret/line-fragment height is the font's lineHeight (measured 17.0),
                // not asc-desc+lead (16.49): centering by the latter leaves the 17pt-tall
                // fragment/caret center at 20.25, 0.25pt off the field's center.
                let line_h: f64 = msg_send![font, lineHeight];
                if line_h > 0.0 && line_h < rect.size.height {
                    // 垂直居中必须相对"编辑器顶部在字段坐标中的位置"计算:输入态文字顶边
                    // = 编辑器顶边相对字段上沿的偏移 + inset,行框中心应落在字段垂直中心。
                    // 之前按 frame 高度差做**加法**补偿,会把共享 field editor 每次聚焦
                    // 撑高 2/4/8/16pt(inset 越大越长,正反馈),文字逐次下移且越来越偏。
                    // 改为减去编辑器顶部偏移(convertRect 换算到字段坐标系),inset 随
                    // 编辑器实际位置自我校正,文字稳定居中,也不再触发编辑器长高。
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
                    // 编辑器顶部相对字段上沿的偏移(非翻转字段坐标:顶部 = maxY)。
                    // How far the editor's top sits above the field's top (non-flipped:
                    // the top edge is maxY).
                    let top_offset = in_field.origin.y + in_field.size.height - rect.size.height;
                    let vertical = ((rect.size.height - line_h) / 2.0 - top_offset).max(0.0);
                    let _: () = msg_send![
                        editor as *mut AnyObject,
                        setTextContainerInset: NSSize::new(SEARCH_PAD_IN + SEARCH_ICON_W, vertical)
                    ];
                    // NSTextContainer 默认 lineFragmentPadding = 2,会把输入文字额外右推 2pt,
                    // 与失焦后手绘保留查询的起点(12 + 22 = 34)不一致 → 按 ↓ 时文字左跳 2pt。
                    // 置零让字形从容器原点(即 inset 起点)开始,与手绘坐标完全一致。
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
