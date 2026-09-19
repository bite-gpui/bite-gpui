//! The scene primitives this fork deviates on.
//!
//! `gpui_types` carries upstream's `Shadow`, whose inset flag is a `u32`. This
//! fork keeps that flag explicit in the storage-buffer ABI instead, so its
//! `Shadow` and the `ShaderBool` it uses live here. `gpui` re-exports these in
//! place of upstream's, which is what leaves the fork's own surface unchanged,
//! and the renderer crates (`gpui_render`, `gpui_wgpu`, `gpui_windows`) can name
//! them without depending on `gpui`.

use gpui_types::{Bounds, ContentMask, Corners, DrawOrder, Hsla, ScaledPixels};

/// A boolean with the same four-byte representation in Rust and WGSL.
/// Scene structs use it over one-byte [`bool`] to keep the storage-buffer ABI explicit.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum ShaderBool {
    /// The flag is disabled.
    #[default]
    Disabled = 0,
    /// The flag is enabled.
    Enabled = 1,
}

impl ShaderBool {
    /// Returns this flag as a regular Rust boolean.
    pub fn is_enabled(self) -> bool {
        self == Self::Enabled
    }
}

impl From<bool> for ShaderBool {
    fn from(value: bool) -> Self {
        if value { Self::Enabled } else { Self::Disabled }
    }
}

/// A shadow to paint, with its inset flag spelled the way the storage buffer expects it.
#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub struct Shadow {
    pub order: DrawOrder,
    pub blur_radius: ScaledPixels,
    pub bounds: Bounds<ScaledPixels>,
    pub corner_radii: Corners<ScaledPixels>,
    pub content_mask: ContentMask<ScaledPixels>,
    pub color: Hsla,
    pub element_bounds: Bounds<ScaledPixels>,
    pub element_corner_radii: Corners<ScaledPixels>,
    /// Whether this shadow is rendered inside the element instead of outside it.
    pub inset: ShaderBool,
    pub padding: u32,
}
