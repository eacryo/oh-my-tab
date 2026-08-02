//! 鼠标增强功能模块(对应 docs/mouse-architecture.md 的设计)。
//! 提供滚轮两分支模式:默认(透传+可反转)/按行(固定行数)。
//!
//! Mouse enhancement module (per docs/mouse-architecture.md design).
//! Provides two scroll modes: Default (passthrough + optional reverse) and Line (fixed line
//! count).

pub(crate) mod device;
pub(crate) mod event_tap;
pub(crate) mod ffi;
pub(crate) mod pointer;
pub(crate) mod resolve;
pub(crate) mod scrolling;

pub(crate) use event_tap::start;
