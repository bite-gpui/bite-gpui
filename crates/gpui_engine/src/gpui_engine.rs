//! The scene/paint engine that sits below the `gpui` facade.
//!
//! This crate holds the scene representation ([`Scene`] and its draw primitives), the
//! sprite-atlas vocabulary, the graphical filters, and the identifiers and raster parameters
//! the scene references, so a platform backend or an alternative renderer can consume the
//! scene without depending on `gpui` itself. It also holds the font and text-run vocabulary
//! that the text machinery and its backends share, so a backend can name a font and return
//! shaped runs without depending on `gpui`. It pulls in no windowing, layout, or GPU driver
//! code; its only target-specific dependencies are the native capture handles that
//! [`SurfaceSource`] carries on macOS and Windows.
//!
//! `gpui` re-exports everything in this crate, so `gpui::Scene` and
//! `gpui_engine::Scene` name the same type and consumers do not need to know this crate
//! exists.

#![warn(missing_docs)]
// Mirrors `gpui`: the moved code carries a few `mut` bindings that only some platform
// cfgs make redundant.
#![allow(unused_mut)]

mod atlas;
mod bounds_tree;
// Public because `gpui`'s `style` module re-exports `Filter`/`ScaledFilter` through this
// path; every other item in the crate is re-exported flat at the root.
pub mod filter;
mod font_fallbacks;
mod font_features;
mod render;
mod renderer;
mod scene;
mod surface;
mod text;

pub use atlas::*;
pub use filter::*;
pub use font_fallbacks::*;
pub use font_features::*;
pub use render::*;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
pub use renderer::*;
pub use scene::*;
pub use surface::*;
pub use text::*;
