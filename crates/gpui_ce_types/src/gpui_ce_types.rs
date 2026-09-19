//! gpui-ce's extensions to the upstream API primitives.
//!
//! The rule this crate keeps: a crate named after upstream (`gpui_types`,
//! `gpui_platform`, ...) carries upstream's public surface unchanged, so any fork's
//! consumers can install it and see what they expect. Anything a fork adds - or
//! changes in a way that moves a signature or a member - lives here instead, in a
//! crate whose name carries the fork marker, and `gpui` re-exports both, which is
//! what leaves the fork's own public surface unchanged.
//!
//! What it holds today: the `palette`-based color extension, and this fork's scene
//! primitives (`ShaderBool` and its `Shadow`).
//!
//! The upstream [`gpui_types::color`] API is the canonical color API re-exported
//! by `gpui`. This crate keeps gpui-ce's `palette` dependency as an extension:
//! the [`palette`] crate itself is re-exported, and conversions between `palette`
//! and the upstream color types, along with gpui-ce's own extras ([`ColorExt`],
//! [`IntoHsla`], [`rgb_to_hsla`]) and the pre-migration back-compat shims live
//! here.

mod compat;
mod extras;
mod interop;
mod scene;

pub use compat::*;
pub use extras::*;
pub use interop::*;
pub use scene::*;
pub use palette;
