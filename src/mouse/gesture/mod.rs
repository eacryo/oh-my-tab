//! Mouse gestures: hold a button, draw a direction sequence, release to run a binding.
//!
//! The event model is *take the press, replay it when it was not a gesture*. Real macOS gesture
//! apps (MacGesture, Strokly) do the same: a button's down is swallowed immediately, because an app
//! that receives it acts inside \`rightMouseDown\` and the decision cannot be undone afterwards. When
//! the press turns out to be a plain click, the pair is replayed so the context menu appears as
//! usual.
//!
//! Layering: [[recognizer]] and [[engine]] are pure and unit-tested; [[table]] holds the published
//! binding snapshot; the tap thread only runs bounded state transitions, and everything that touches
//! AppKit happens on the main thread.

pub(crate) mod conflict;
pub(crate) mod engine;
pub(crate) mod executor;
pub(crate) mod hud;
pub(crate) mod modifiers;
pub(crate) mod recognizer;
pub(crate) mod recording;
pub(crate) mod replay;
pub(crate) mod runtime;
pub(crate) mod session_state;
pub(crate) mod switcher;
pub(crate) mod table;
