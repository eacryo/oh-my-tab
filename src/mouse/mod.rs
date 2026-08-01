//! 鼠标增强功能模块(对应 docs/mouse-architecture.md 的设计)。
//! 提供滚轮三分支模式:默认(透传+可反转)/按行(固定行数)/平滑(物理引擎+惯性)。
//!
//! Mouse enhancement module (per docs/mouse-architecture.md design).
//! Provides three scroll modes: Default (passthrough + optional reverse), Line (fixed line
//! count), and Smooth (physics engine + inertia).

pub(crate) mod device;
pub(crate) mod event_tap;
pub(crate) mod ffi;
pub(crate) mod pointer;
pub(crate) mod resolve;
pub(crate) mod scrolling;

pub(crate) use event_tap::start;
