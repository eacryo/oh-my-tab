//! The Metal renderer: one instanced-quad pipeline over `Inst`, the layout the
//! shader in `shader.rs` reads. The CPU side builds a `Scene` in points; the
//! encoder batches instances by texture so a whole frame is a handful of draws,
//! the way mygo's `internal/gpu/metal` presents a scene.

use super::shader::SHADER_SOURCE;
use super::sys::*;
use crate::ffi::{make_nsstring, objc_msgSend, release_obj, CFRelease};
use objc2::runtime::{AnyObject, Sel};
use objc2::{class, msg_send, sel};
use std::collections::HashMap;
use std::ffi::c_void;

/// Textures bound per draw batch; `MAX_TEX` in the shader must agree.
pub(crate) const MAX_TEX: usize = 16;

/// One scene op, as the shader's `struct Inst` reads it (112 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Inst {
    rect: [f32; 4],
    radii: [f32; 4],
    color: [f32; 4],
    color2: [f32; 4],
    uv: [f32; 4],
    clip: [f32; 4],
    misc: [f32; 4],
}

const KIND_FILL: f32 = 0.0;
const KIND_BORDER: f32 = 1.0;
const KIND_IMAGE: f32 = 2.0;
const KIND_GLYPH: f32 = 3.0;

/// rgba(hex) as straight-alpha floats; `hex` is the codebase's 0xRRGGBBAA.
pub(crate) fn f4(hex: u32) -> [f32; 4] {
    [
        ((hex >> 24) & 0xff) as f32 / 255.0,
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ]
}

fn premul(c: [f32; 4]) -> [f32; 4] {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}

/// Points in, premultiplied colors in, device pixels out (the scale bakes into
/// every emitted instance).
pub(crate) struct Scene {
    scale: f32,
    insts: Vec<Inst>,
    textures: Vec<*mut AnyObject>,
    tex_map: HashMap<usize, u32>,
    clip_stack: Vec<[f32; 4]>,
    max_clip: [f32; 4],
}

impl Scene {
    pub(crate) fn new(scale: f64, viewport_w: f64, viewport_h: f64) -> Self {
        let s = scale as f32;
        Self {
            scale: s,
            insts: Vec::with_capacity(512),
            textures: Vec::new(),
            tex_map: HashMap::new(),
            clip_stack: Vec::new(),
            max_clip: [0.0, 0.0, (viewport_w * scale) as f32, (viewport_h * scale) as f32],
        }
    }

    fn clip(&self) -> [f32; 4] {
        self.clip_stack.last().copied().unwrap_or(self.max_clip)
    }

    /// Run `f` with `rect` (points) intersected into the current clip.
    pub(crate) fn with_clip<R>(&mut self, rect: [f64; 4], f: impl FnOnce(&mut Scene) -> R) -> R {
        let c = self.clip();
        let x0 = (rect[0] * self.scale as f64).max(c[0] as f64) as f32;
        let y0 = (rect[1] * self.scale as f64).max(c[1] as f64) as f32;
        let x1 = ((rect[0] + rect[2]) * self.scale as f64).min((c[0] + c[2]) as f64) as f32;
        let y1 = ((rect[1] + rect[3]) * self.scale as f64).min((c[1] + c[3]) as f64) as f32;
        self.clip_stack.push([x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]);
        let r = f(self);
        self.clip_stack.pop();
        r
    }

    fn px(&self, r: [f64; 4]) -> [f32; 4] {
        let s = self.scale;
        [
            (r[0] * s as f64) as f32,
            (r[1] * s as f64) as f32,
            (r[2] * s as f64) as f32,
            (r[3] * s as f64) as f32,
        ]
    }

    fn texture_id(&mut self, tex: *mut AnyObject) -> f32 {
        if let Some(&idx) = self.tex_map.get(&(tex as usize)) {
            return idx as f32;
        }
        let idx = self.textures.len() as u32;
        self.textures.push(tex);
        self.tex_map.insert(tex as usize, idx);
        idx as f32
    }

    /// A solid rounded rect (radius in points; per-corner via `fill_radii`).
    pub(crate) fn fill(&mut self, rect: [f64; 4], radius: f64, color: [f32; 4]) {
        self.fill_radii(rect, [radius; 4], color);
    }

    pub(crate) fn fill_radii(&mut self, rect: [f64; 4], radii: [f64; 4], color: [f32; 4]) {
        if color[3] <= 0.0 || rect[2] <= 0.0 || rect[3] <= 0.0 {
            return;
        }
        let s = self.scale;
        self.insts.push(Inst {
            rect: self.px(rect),
            radii: radii.map(|r| (r * s as f64) as f32),
            color: premul(color),
            color2: [0.0; 4],
            uv: [0.0; 4],
            clip: self.clip(),
            misc: [KIND_FILL, 0.0, 0.0, 1.0],
        });
    }

    /// A rounded rect with an inner border ring of `width` points.
    pub(crate) fn border(
        &mut self,
        rect: [f64; 4],
        radius: f64,
        fill: [f32; 4],
        border: [f32; 4],
        width: f64,
    ) {
        if rect[2] <= 0.0 || rect[3] <= 0.0 {
            return;
        }
        let s = self.scale;
        self.insts.push(Inst {
            rect: self.px(rect),
            radii: [radius; 4].map(|r| (r * s as f64) as f32),
            color: premul(fill),
            color2: premul(border),
            uv: [0.0; 4],
            clip: self.clip(),
            misc: [KIND_BORDER, 0.0, (width * s as f64) as f32, 1.0],
        });
    }

    /// A texture quad (`dest` in points). `mask_tint` Some(color) samples the
    /// texture's alpha and tints it; None draws the premultiplied bitmap.
    pub(crate) fn image(
        &mut self,
        tex: *mut AnyObject,
        dest: [f64; 4],
        opacity: f32,
        mask_tint: Option<[f32; 4]>,
    ) {
        if tex.is_null() || dest[2] <= 0.0 || dest[3] <= 0.0 || opacity <= 0.0 {
            return;
        }
        let kind = if mask_tint.is_some() {
            KIND_GLYPH
        } else {
            KIND_IMAGE
        };
        let idx = self.texture_id(tex);
        self.insts.push(Inst {
            rect: self.px(dest),
            radii: [0.0; 4],
            color: [0.0; 4],
            color2: mask_tint.map(premul).unwrap_or([0.0; 4]),
            uv: [0.0, 0.0, 1.0, 1.0],
            clip: self.clip(),
            misc: [kind, idx, 0.0, opacity],
        });
    }

    fn is_empty(&self) -> bool {
        self.insts.is_empty()
    }
}

/// The GPU side: device, queue, the one pipeline state.
pub(crate) struct Renderer {
    device: *mut AnyObject,
    queue: *mut AnyObject,
    pso: *mut AnyObject,
}

impl Renderer {
    /// Build the device/queue/pipeline once. None when Metal is unavailable or the
    /// shader fails to compile (the NSError's text is logged).
    pub(crate) unsafe fn new() -> Option<Renderer> {
        let device = MTLCreateSystemDefaultDevice();
        if device.is_null() {
            crate::log_info!("[metal] no system default device");
            return None;
        }
        let _: *mut AnyObject = msg_send![device, retain];
        let queue: *mut AnyObject = msg_send![device, newCommandQueue];
        if queue.is_null() {
            crate::log_info!("[metal] newCommandQueue failed");
            return None;
        }
        let source = make_nsstring(SHADER_SOURCE);
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let library: *mut AnyObject = msg_send![
            device,
            newLibraryWithSource: source,
            options: std::ptr::null::<AnyObject>(),
            error: &mut error as *mut *mut AnyObject
        ];
        CFRelease(source as *const c_void);
        if library.is_null() {
            let desc: *mut AnyObject = if error.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![error, localizedDescription]
            };
            let text = if desc.is_null() {
                "unknown".to_owned()
            } else {
                crate::ffi::nsstring_to_rust(desc)
            };
            crate::log_info!("[metal] shader compile failed: {}", text);
            return None;
        }
        let vs_name = make_nsstring("vs");
        let fs_name = make_nsstring("fs");
        let vs: *mut AnyObject = msg_send![library, newFunctionWithName: vs_name];
        let fs: *mut AnyObject = msg_send![library, newFunctionWithName: fs_name];
        CFRelease(vs_name as *const c_void);
        CFRelease(fs_name as *const c_void);
        if vs.is_null() || fs.is_null() {
            crate::log_info!("[metal] entry points missing");
            release_obj(vs);
            release_obj(fs);
            release_obj(library);
            return None;
        }
        let desc: *mut AnyObject = msg_send![class!(MTLRenderPipelineDescriptor), new];
        let _: () = msg_send![desc, setVertexFunction: vs];
        let _: () = msg_send![desc, setFragmentFunction: fs];
        let attachments: *mut AnyObject = msg_send![desc, colorAttachments];
        let att0: *mut AnyObject = msg_send![attachments, objectAtIndexedSubscript: 0usize];
        let _: () = msg_send![att0, setPixelFormat: MTL_PIXEL_FORMAT_BGRA8_UNORM];
        let _: () = msg_send![att0, setBlendingEnabled: true];
        let _: () = msg_send![att0, setSourceRGBBlendFactor: MTL_BLEND_FACTOR_ONE];
        let _: () = msg_send![att0, setDestinationRGBBlendFactor: MTL_BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA];
        let _: () = msg_send![att0, setSourceAlphaBlendFactor: MTL_BLEND_FACTOR_ONE];
        let _: () = msg_send![att0, setDestinationAlphaBlendFactor: MTL_BLEND_FACTOR_ONE_MINUS_SOURCE_ALPHA];
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let pso: *mut AnyObject =
            msg_send![device, newRenderPipelineStateWithDescriptor: desc, error: &mut error as *mut *mut AnyObject];
        release_obj(desc);
        release_obj(vs);
        release_obj(fs);
        release_obj(library);
        if pso.is_null() {
            let desc: *mut AnyObject = if error.is_null() {
                std::ptr::null_mut()
            } else {
                msg_send![error, localizedDescription]
            };
            let text = if desc.is_null() {
                "unknown".to_owned()
            } else {
                crate::ffi::nsstring_to_rust(desc)
            };
            crate::log_info!("[metal] pipeline failed: {}", text);
            return None;
        }
        Some(Renderer {
            device,
            queue,
            pso,
        })
    }

    /// The layer's device must match the renderer's (one GPU in practice).
    pub(crate) fn device(&self) -> *mut AnyObject {
        self.device
    }

    /// Upload BGRA premultiplied pixels into a new texture (caller releases the
    /// returned +1 object when done with it — the cache retains it instead).
    pub(crate) unsafe fn make_texture(
        &self,
        width: usize,
        height: usize,
        pixels: &[u8],
    ) -> *mut AnyObject {
        if width == 0 || height == 0 || pixels.len() < width * height * 4 {
            return std::ptr::null_mut();
        }
        let desc: *mut AnyObject = msg_send![
            class!(MTLTextureDescriptor),
            texture2DDescriptorWithPixelFormat: MTL_PIXEL_FORMAT_RGBA8_UNORM,
            width: width,
            height: height,
            mipmapped: false
        ];
        let _: () = msg_send![desc, setUsage: MTL_TEXTURE_USAGE_SHADER_READ];
        let texture: *mut AnyObject = msg_send![self.device, newTextureWithDescriptor: desc];
        if texture.is_null() {
            return texture;
        }
        // MTLRegion {origin, size} of 6 u64 — a struct arg, so the raw sender.
        #[repr(C)]
        struct Region {
            ox: u64,
            oy: u64,
            oz: u64,
            w: u64,
            h: u64,
            d: u64,
        }
        let region = Region {
            ox: 0,
            oy: 0,
            oz: 0,
            w: width as u64,
            h: height as u64,
            d: 1,
        };
        type F = unsafe extern "C" fn(*mut AnyObject, Sel, Region, u64, *const c_void, u64);
        let f: F = std::mem::transmute(objc_msgSend as *const ());
        f(
            texture,
            sel!(replaceRegion:mipmapLevel:withBytes:bytesPerRow:),
            region,
            0,
            pixels.as_ptr() as *const c_void,
            (width * 4) as u64,
        );
        texture
    }

    /// Encode and present one frame on `layer` (a CAMetalLayer). Safe to call
    /// whenever the scene changes; the drawable is acquired on the spot.
    pub(crate) unsafe fn render(&self, layer: *mut AnyObject, scene: &Scene) {
        if layer.is_null() || scene.is_empty() {
            crate::log_info!(
                "[metal] render skipped: layer_null={} insts={}",
                layer.is_null(),
                scene.insts.len()
            );
            return;
        }
        let drawable: *mut AnyObject = msg_send![layer, nextDrawable];
        if drawable.is_null() {
            let is_metal: bool =
                msg_send![layer, isKindOfClass: class!(CAMetalLayer)];
            let ds: objc2_foundation::NSSize = msg_send![layer, drawableSize];
            crate::log_info!(
                "[metal] nextDrawable nil: cametal={} drawable={}x{}",
                is_metal,
                ds.width,
                ds.height
            );
            return;
        }
        let drawable_texture: *mut AnyObject = msg_send![drawable, texture];
        let desc: *mut AnyObject = msg_send![class!(MTLRenderPassDescriptor), renderPassDescriptor];
        let attachments: *mut AnyObject = msg_send![desc, colorAttachments];
        let att0: *mut AnyObject = msg_send![attachments, objectAtIndexedSubscript: 0usize];
        let _: () = msg_send![att0, setTexture: drawable_texture];
        let _: () = msg_send![att0, setLoadAction: MTL_LOAD_ACTION_CLEAR];
        let _: () = msg_send![att0, setStoreAction: MTL_STORE_ACTION_STORE];
        let clear = MTLClearColor {
            red: 0.0,
            green: 0.0,
            blue: 0.0,
            alpha: 0.0,
        };
        type SetClear = unsafe extern "C" fn(*mut AnyObject, Sel, MTLClearColor);
        let f: SetClear = std::mem::transmute(objc_msgSend as *const ());
        f(att0, sel!(setClearColor:), clear);

        let command_buffer: *mut AnyObject = msg_send![self.queue, commandBuffer];
        let encoder: *mut AnyObject =
            msg_send![command_buffer, renderCommandEncoderWithDescriptor: desc];
        if encoder.is_null() {
            return;
        }
        let _: () = msg_send![encoder, setRenderPipelineState: self.pso];

        // globals: the viewport in pixels (drawable size), x,y used by the vs.
        let drawable_size: objc2_foundation::NSSize = msg_send![layer, drawableSize];
        let drawable_w = drawable_size.width.max(1.0);
        let drawable_h = drawable_size.height.max(1.0);
        let globals: [f32; 4] = [drawable_w as f32, drawable_h as f32, 0.0, 0.0];
        let globals_buf: *mut AnyObject = msg_send![
            self.device,
            newBufferWithBytes: globals.as_ptr() as *const c_void,
            length: 16usize,
            options: 0u64
        ];

        // Batch the op list: order-preserving chunks of at most MAX_TEX textures.
        let mut start = 0usize;
        while start < scene.insts.len() {
            let mut local: Vec<*mut AnyObject> = Vec::with_capacity(MAX_TEX);
            let mut remap: HashMap<u32, u32> = HashMap::new();
            let mut end = start;
            while end < scene.insts.len() {
                let inst = &scene.insts[end];
                let tex_idx = inst.misc[1] as u32;
                let needs_tex = inst.misc[0] >= KIND_IMAGE;
                if needs_tex {
                    if let std::collections::hash_map::Entry::Vacant(e) = remap.entry(tex_idx) {
                        if local.len() >= MAX_TEX {
                            break;
                        }
                        e.insert(local.len() as u32);
                        local.push(scene.textures[tex_idx as usize]);
                    }
                }
                end += 1;
            }
            let batch: Vec<Inst> = scene.insts[start..end]
                .iter()
                .map(|inst| {
                    let mut i = *inst;
                    if i.misc[0] >= KIND_IMAGE {
                        i.misc[1] = remap[&(i.misc[1] as u32)] as f32;
                    }
                    i
                })
                .collect();
            let buf: *mut AnyObject = msg_send![
                self.device,
                newBufferWithBytes: batch.as_ptr() as *const c_void,
                length: batch.len() * std::mem::size_of::<Inst>(),
                options: 0u64
            ];
            let _: () = msg_send![encoder, setVertexBuffer: buf, offset: 0usize, atIndex: 0usize];
            let _: () =
                msg_send![encoder, setVertexBuffer: globals_buf, offset: 0usize, atIndex: 1usize];
            // The fragment stage reads the same instance table (rect, colors,
            // clip, misc) — it is not the vertex stage's bindings.
            let _: () = msg_send![encoder, setFragmentBuffer: buf, offset: 0usize, atIndex: 0usize];
            for (slot, &texture) in local.iter().enumerate() {
                let _: () =
                    msg_send![encoder, setFragmentTexture: texture, atIndex: slot];
            }
            let _: () = msg_send![
                encoder,
                drawPrimitives: MTL_PRIMITIVE_TRIANGLE_STRIP,
                vertexStart: 0usize,
                vertexCount: 4usize,
                instanceCount: batch.len()
            ];
            release_obj(buf);
            start = end;
        }
        release_obj(globals_buf);
        let _: () = msg_send![encoder, endEncoding];
        let _: () = msg_send![command_buffer, presentDrawable: drawable];
        let _: () = msg_send![command_buffer, commit];
    }
}
