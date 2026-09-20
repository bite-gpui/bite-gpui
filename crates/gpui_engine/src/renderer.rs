//! The renderer contract a headless platform backend implements.

// `PlatformHeadlessRenderer` is only present when tests, benches, or an offscreen
// renderer need it, so its imports carry the same gate.
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
use crate::{PlatformAtlas, Scene};
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
use anyhow::Result;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
use gpui_types::{DevicePixels, Size};
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
use image::RgbaImage;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
use std::sync::Arc;

/// A renderer for headless windows that can produce real rendered output.
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
pub trait PlatformHeadlessRenderer {
    /// Render a scene and return the result as an RGBA image.
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> Result<RgbaImage>;

    /// Render a scene to an offscreen target without reading the result back.
    ///
    /// This is the headless analogue of presenting a frame: it performs the
    /// same CPU-side scene encoding and GPU submission as drawing to a real
    /// window, but doesn't block on GPU completion or copy pixels back.
    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> Result<()>;

    /// Returns the sprite atlas used by this renderer.
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas>;
}
