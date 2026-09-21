//! The element-side gesture vocabulary and the portable touch recognizer now
//! live in `gpui_authoring`; this module re-exports them so every
//! `crate::gestures::*` path — and `gpui::OngoingScroll` — is unchanged.
pub use gpui_authoring::gestures::*;
