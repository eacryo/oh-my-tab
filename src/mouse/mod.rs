//! 鼠标增强功能模块(对应 docs/mouse-architecture.md 的设计)。
//! 当前为最小验证阶段:仅监听鼠标按键/滚轮事件并输出日志,验证 event tap 链路。
//!
//! Mouse enhancement module (per docs/mouse-architecture.md design).
//! Currently a minimal verification: only listens for mouse button/scroll events and logs them,
//! validating the event tap pipeline.

pub(crate) mod event_tap;

pub(crate) use event_tap::start;
