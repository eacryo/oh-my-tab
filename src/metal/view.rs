//! `OhMyTabMetalView`: an NSView whose backing layer is a CAMetalLayer and whose
//! whole content is drawn by the Metal renderer — the same "the app draws
//! everything itself" surface mygo's native UI puts behind its windows. Input
//! goes to registered handlers; nothing AppKit owns sits inside the view.

use super::render::Renderer;
use super::sys::*;
use crate::callback_guard;
use crate::ffi::{MainThreadSlot, StaticClass};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use std::ffi::c_void;
use std::sync::OnceLock;

/// What a surface asks the view to do. `render` rebuilds the scene and presents
/// it; the point handlers receive the event's location already converted to the
/// view's (flipped, top-left) coordinate space.
pub(crate) struct MetalHandlers {
    pub(crate) render: unsafe fn(*mut AnyObject, NSRect),
    pub(crate) key_down: Option<unsafe fn(*mut AnyObject)>,
    pub(crate) mouse_moved: Option<unsafe fn(*mut AnyObject, NSPoint)>,
    pub(crate) mouse_down: Option<unsafe fn(*mut AnyObject, NSPoint)>,
    pub(crate) mouse_up: Option<unsafe fn(*mut AnyObject, NSPoint)>,
    pub(crate) mouse_exited: Option<unsafe fn(*mut AnyObject)>,
    pub(crate) scroll_wheel: Option<unsafe fn(*mut AnyObject, f64, f64)>,
}

static HANDLERS: MainThreadSlot<Option<MetalHandlers>> = unsafe { MainThreadSlot::new(None) };
/// The live view pointer for the callbacks (one metal surface at a time).
static VIEW: MainThreadSlot<Option<*mut AnyObject>> = unsafe { MainThreadSlot::new(None) };
static RENDERER: MainThreadSlot<Option<Renderer>> = unsafe { MainThreadSlot::new(None) };

/// Register the surface callbacks and the shared renderer, once.
pub(crate) unsafe fn install_handlers(handlers: MetalHandlers) {
    *HANDLERS.lock().unwrap() = Some(handlers);
}

/// The shared renderer as a raw pointer (valid after `shared_renderer` ran once;
/// the slot never moves it). Main-thread use only, like everything else here.
pub(crate) fn renderer_ptr() -> *const Renderer {
    match *RENDERER.lock().unwrap() {
        Some(ref r) => r as *const Renderer,
        None => std::ptr::null(),
    }
}

fn handlers<R>(f: impl FnOnce(&MetalHandlers) -> R) -> Option<R> {
    HANDLERS.lock().unwrap().as_ref().map(f)
}

/// Create the Metal view inside `parent` covering `frame`. Returns the view.
pub(crate) unsafe fn create_view(parent: *mut AnyObject, frame: NSRect) -> *mut AnyObject {
    let cls = view_class();
    let view: *mut AnyObject = msg_send![cls.0 as *mut AnyObject, alloc];
    let view: *mut AnyObject = msg_send![view, initWithFrame: frame];
    let _: () = msg_send![view, setWantsLayer: true];
    let _: () = msg_send![parent, addSubview: view];
    *VIEW.lock().unwrap() = Some(view);
    view
}

/// Redraw right now (scroll, hover, model change) instead of waiting for the
/// display cycle; also marks the view dirty so the next AppKit pass agrees.
pub(crate) unsafe fn redraw() {
    if let Some(view) = *VIEW.lock().unwrap() {
        let bounds: NSRect = msg_send![view, bounds];
        if let Some(render) = handlers(|h| h.render) {
            render(view, bounds);
        }
        let _: () = msg_send![view, setNeedsDisplay: true];
    }
}

/// The renderer the surface rasterizes with, lazily built.
pub(crate) unsafe fn shared_renderer() -> Option<*const Renderer> {
    if RENDERER.lock().unwrap().is_none() {
        *RENDERER.lock().unwrap() = Renderer::new();
    }
    let ptr = renderer_ptr();
    if ptr.is_null() {
        None
    } else {
        Some(ptr)
    }
}

/// Update the CAMetalLayer's scale/drawableSize from the view's live geometry.
unsafe fn sync_drawable(view: *mut AnyObject) {
    let layer: *mut AnyObject = msg_send![view, layer];
    if layer.is_null() {
        return;
    }
    let window: *mut AnyObject = msg_send![view, window];
    let scale: f64 = if window.is_null() {
        1.0
    } else {
        msg_send![window, backingScaleFactor]
    };
    let bounds: NSRect = msg_send![view, bounds];
    let _: () = msg_send![layer, setContentsScale: scale];
    let _: () = msg_send![
        layer,
        setDrawableSize: NSSize::new(
            (bounds.size.width * scale).max(1.0),
            (bounds.size.height * scale).max(1.0)
        )
    ];
}

fn view_class() -> StaticClass {
    static CLS: OnceLock<StaticClass> = OnceLock::new();
    crate::ffi::StaticClass::register_once(&CLS, c"OhMyTabMetalView", || {
        crate::ffi::build_class(c"OhMyTabMetalView", class!(NSView), |builder| unsafe {
            builder.add_method::<AnyObject, _>(
                sel!(makeBackingLayer),
                make_backing_layer as extern "C" fn(*mut AnyObject, Sel) -> *mut AnyObject,
            );
            builder.add_method::<AnyObject, _>(
                sel!(isFlipped),
                is_flipped as extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
            builder.add_method::<AnyObject, _>(
                sel!(acceptsFirstResponder),
                accepts_first as extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
            builder.add_method::<AnyObject, _>(
                sel!(wantsUpdateLayer),
                wants_update as extern "C" fn(*mut AnyObject, Sel) -> objc2::runtime::Bool,
            );
            builder.add_method::<AnyObject, _>(
                sel!(updateLayer),
                update_layer as extern "C" fn(*mut AnyObject, Sel),
            );
            builder.add_method::<AnyObject, _>(
                sel!(updateTrackingAreas),
                update_tracking as extern "C" fn(*mut AnyObject, Sel),
            );
            builder.add_method::<AnyObject, _>(
                sel!(viewDidChangeBackingProperties),
                backing_changed as extern "C" fn(*mut AnyObject, Sel),
            );
            builder.add_method::<AnyObject, _>(
                sel!(setFrameSize:),
                frame_changed as extern "C" fn(*mut AnyObject, Sel, NSSize),
            );
            builder.add_method::<AnyObject, _>(
                sel!(keyDown:),
                key_down_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            builder.add_method::<AnyObject, _>(
                sel!(mouseDown:),
                mouse_down_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            builder.add_method::<AnyObject, _>(
                sel!(mouseUp:),
                mouse_up_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            builder.add_method::<AnyObject, _>(
                sel!(mouseMoved:),
                mouse_moved_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            builder.add_method::<AnyObject, _>(
                sel!(mouseDragged:),
                mouse_moved_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            builder.add_method::<AnyObject, _>(
                sel!(mouseExited:),
                mouse_exited_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
            builder.add_method::<AnyObject, _>(
                sel!(scrollWheel:),
                scroll_cb as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject),
            );
        })
    })
}

extern "C" fn make_backing_layer(_self: *mut AnyObject, _cmd: Sel) -> *mut AnyObject {
    unsafe {
        let layer: *mut AnyObject = msg_send![class!(CAMetalLayer), new];
        if let Some(renderer) = shared_renderer() {
            let _: () = msg_send![layer, setDevice: (*renderer).device()];
        }
        let _: () = msg_send![layer, setPixelFormat: MTL_PIXEL_FORMAT_BGRA8_UNORM];
        // Non-opaque so the glass backdrop shows through; framebufferOnly is fine
        // (nothing samples the drawable).
        let _: () = msg_send![layer, setOpaque: false];
        let _: () = msg_send![layer, setNeedsDisplayOnBoundsChange: true];
        layer
    }
}

extern "C" fn is_flipped(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    objc2::runtime::Bool::YES
}

extern "C" fn accepts_first(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    objc2::runtime::Bool::YES
}

extern "C" fn wants_update(_self: *mut AnyObject, _cmd: Sel) -> objc2::runtime::Bool {
    objc2::runtime::Bool::YES
}

extern "C" fn update_layer(_self: *mut AnyObject, _cmd: Sel) {
    callback_guard::void("metal_update_layer", || unsafe {
        sync_drawable(_self);
        let bounds: NSRect = msg_send![_self, bounds];
        if let Some(render) = handlers(|h| h.render) {
            render(_self, bounds);
        }
    });
}

extern "C" fn update_tracking(_self: *mut AnyObject, _cmd: Sel) {
    callback_guard::void("metal_update_tracking", || unsafe {
        let areas: *mut AnyObject = msg_send![_self, trackingAreas];
        let count: usize = msg_send![areas, count];
        for i in (0..count).rev() {
            let area: *mut AnyObject = msg_send![areas, objectAtIndex: i];
            let _: () = msg_send![_self, removeTrackingArea: area];
        }
        let area: *mut AnyObject = msg_send![class!(NSTrackingArea), alloc];
        // entered/exited + moved + always + in-visible-rect (same mask the picker uses).
        let area: *mut AnyObject = msg_send![
            area,
            initWithRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            options: 0x283u64,
            owner: _self,
            userInfo: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![_self, addTrackingArea: area];
        crate::ffi::release_obj(area);
    });
}

extern "C" fn backing_changed(_self: *mut AnyObject, _cmd: Sel) {
    callback_guard::void("metal_backing_changed", || unsafe {
        sync_drawable(_self);
        let bounds: NSRect = msg_send![_self, bounds];
        if let Some(render) = handlers(|h| h.render) {
            render(_self, bounds);
        }
    });
}

extern "C" fn frame_changed(_self: *mut AnyObject, _cmd: Sel, size: NSSize) {
    // super first, then re-fit the drawable.
    callback_guard::void("metal_frame_changed", || unsafe {
        type F = unsafe extern "C" fn(*mut crate::ffi::ObjcSuper, Sel, NSSize);
        let mut sup = crate::ffi::ObjcSuper::for_defined_class(_self as *mut c_void, view_class());
        let f: F = std::mem::transmute(crate::ffi::objc_msgSendSuper as *const ());
        f(&mut sup, sel!(setFrameSize:), size);
        sync_drawable(_self);
        let bounds: NSRect = msg_send![_self, bounds];
        if let Some(render) = handlers(|h| h.render) {
            render(_self, bounds);
        }
    });
}

extern "C" fn key_down_cb(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("metal_key_down", || unsafe {
        if let Some(cb) = handlers(|h| h.key_down).flatten() {
            cb(event);
        }
    });
}

fn view_point(view: *mut AnyObject, event: *mut AnyObject) -> NSPoint {
    unsafe {
        let win: NSPoint = msg_send![event, locationInWindow];
        msg_send![view, convertPoint: win, fromView: std::ptr::null::<AnyObject>()]
    }
}

extern "C" fn mouse_down_cb(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("metal_mouse_down", || unsafe {
        if let Some(cb) = handlers(|h| h.mouse_down).flatten() {
            cb(event, view_point(_self, event));
        }
    });
}

extern "C" fn mouse_up_cb(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("metal_mouse_up", || unsafe {
        if let Some(cb) = handlers(|h| h.mouse_up).flatten() {
            cb(event, view_point(_self, event));
        }
    });
}

extern "C" fn mouse_moved_cb(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("metal_mouse_moved", || unsafe {
        if let Some(cb) = handlers(|h| h.mouse_moved).flatten() {
            cb(event, view_point(_self, event));
        }
    });
}

extern "C" fn mouse_exited_cb(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("metal_mouse_exited", || unsafe {
        if let Some(cb) = handlers(|h| h.mouse_exited).flatten() {
            cb(event);
        }
    });
}

extern "C" fn scroll_cb(_self: *mut AnyObject, _cmd: Sel, event: *mut AnyObject) {
    callback_guard::void("metal_scroll", || unsafe {
        if let Some(cb) = handlers(|h| h.scroll_wheel).flatten() {
            let dx: f64 = msg_send![event, scrollingDeltaX];
            let dy: f64 = msg_send![event, scrollingDeltaY];
            let precise: bool = msg_send![event, hasPreciseScrollingDeltas];
            let (dx, dy) = if precise { (dx, dy) } else { (dx * 40.0, dy * 40.0) };
            cb(event, dx, dy);
        }
    });
}
