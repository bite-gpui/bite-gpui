//! GPUI's default engine: Taffy-backed layout evaluation.
//!
//! The scene representation and the engine's interface contracts live in
//! [`gpui_engine`]; this crate holds the concrete implementations that depend
//! on `taffy`.
//!
//! The reference also holds the text shaping and wrapping caches here. Those
//! are deferred to the text-layout gate, so for now this crate carries the
//! layout half only.

#![warn(missing_docs)]

mod layout;
mod layout_style;

pub use layout::*;
