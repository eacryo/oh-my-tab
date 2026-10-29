//! Metal-rendered native UI, the Rust counterpart of mygo's "native UI mode"
//! (package `ui` over `internal/gpu/metal`): a CAMetalLayer-backed view whose
//! whole content the app draws itself as instanced quads — rounded-rect fills,
//! borders, bitmap images and text — instead of an AppKit view tree.
//!
//! - `sys`: framework FFI (Metal, QuartzCore, CoreText).
//! - `shader`: the single MSL source, compiled once at startup.
//! - `render`: the instanced renderer and the `Scene` painter.
//! - `text`: CoreText rasterization into cached textures.
//! - `view`: the CAMetalLayer-backed NSView and its input plumbing.

pub(crate) mod render;
pub(crate) mod shader;
pub(crate) mod sys;
pub(crate) mod text;
pub(crate) mod view;
