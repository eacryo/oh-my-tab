//! Shared UI mechanics used by more than one surface. Deliberately NOT a UI framework: it holds
//! the small pieces two subsystems would otherwise each implement (and drift apart on).

pub(crate) mod corner_glyph;
pub(crate) mod toast;
