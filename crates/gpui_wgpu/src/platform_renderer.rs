//! `WgpuRenderer` as a window's renderer.
//!
//! A window holds a `Box<dyn gpui_platform::PlatformRenderer>`; this is how the wgpu renderer
//! answers that contract, for the backends that hold one — wayland and X11 today.
//!
//! It is a module of its own because the impl exists only where the platform's native hooks are
//! not part of the contract. macOS and Windows require `MacSceneRenderer` / `WinSceneRenderer`,
//! which `WgpuRenderer` does not have yet, so those two backends keep their own default until
//! the platform halves of the seam land; the web build still holds its renderer concretely. The
//! module therefore carries one `cfg` — the platforms that hold one, the wayland and X11
//! backends — rather than an item-by-item one.

use anyhow::Result;
use gpui_platform::{DevicePixels, GpuSpecs, PlatformRenderer, RendererTarget, Size};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};

use crate::WgpuRenderer;

impl PlatformRenderer for WgpuRenderer {
    fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        WgpuRenderer::update_drawable_size(self, size);
    }

    fn max_texture_size(&self) -> u32 {
        WgpuRenderer::max_texture_size(self)
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        Some(WgpuRenderer::gpu_specs(self))
    }

    fn set_subpixel_layout(&mut self, is_bgr: bool) {
        WgpuRenderer::set_subpixel_layout(self, is_bgr);
    }

    fn update_transparency(&mut self, transparent: bool) {
        WgpuRenderer::update_transparency(self, transparent);
    }

    fn destroy(&mut self) {
        WgpuRenderer::destroy(self);
    }

    fn device_lost(&self) -> bool {
        WgpuRenderer::device_lost(self)
    }

    fn needs_redraw(&mut self) -> bool {
        WgpuRenderer::needs_redraw(self)
    }

    fn recover(&mut self, target: RendererTarget<'_>) -> Result<()> {
        let window = RawWindowHandles::from_target(&target)?;
        WgpuRenderer::recover(self, &window)
    }
}

/// A window's raw handles, owned, so a renderer can rebuild its surface after a device loss
/// without borrowing the window.
///
/// `WgpuRenderer::new` is handed the window itself, which the backend keeps alive for as long as
/// the renderer exists; recovery only has a [`RendererTarget`], and the handles it carries by
/// value are what the surface is rebuilt from. `WgpuRenderer::recover` still wants a
/// window-shaped value, so this is it.
#[derive(Debug, Clone, Copy)]
struct RawWindowHandles {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

// Safety: the handles name process-global native objects that outlive the renderer, the same
// guarantee the backends' own window wrappers make when they hand their handles to wgpu.
unsafe impl Send for RawWindowHandles {}
unsafe impl Sync for RawWindowHandles {}

impl RawWindowHandles {
    fn from_target(target: &RendererTarget<'_>) -> Result<Self> {
        Ok(Self {
            window: target
                .window_handle
                .ok_or_else(|| anyhow::anyhow!("renderer target has no window handle"))?,
            display: target
                .display_handle
                .ok_or_else(|| anyhow::anyhow!("renderer target has no display handle"))?,
        })
    }
}

impl HasWindowHandle for RawWindowHandles {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for RawWindowHandles {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}
