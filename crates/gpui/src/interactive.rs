//! The element-side input vocabulary, the [`Render`] impl for [`ExternalPaths`] and
//! the input tests all live in the authoring layer; re-exported so `gpui::ClickEvent`
//! and the rest of the input API are unchanged.
pub use gpui_authoring::interactive::*;
