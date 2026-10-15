//! `WgpuRenderer` as a window's renderer.
//!
//! A window holds a `Box<dyn gpui_platform::PlatformRenderer>`; this is how the wgpu renderer
//! answers that contract, for the backends that hold one — wayland and X11 today.
//!
//! macOS is the one platform left out. A window's renderer there must answer `MacSceneRenderer`,
//! whose layer pointer a wgpu renderer cannot supply yet — the wgpu path does not present through a
//! `CAMetalLayer` the window can install as its view's backing layer. The module therefore carries
//! one `cfg` — not wasm, not macOS — instead of an item-by-item one.

use anyhow::Result;
use gpui_engine::GpuRenderer;
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
        WgpuRenderer::gpu_specs(self)
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

impl GpuRenderer for WgpuRenderer {
    /// The shared context slot, which is where a producer on this path reaches the device and the
    /// queue: it is the same `Rc` every window in the process draws through, so reading them out
    /// of it is the rendezvous it exists for. `None` for a renderer built offscreen, which holds
    /// no context because it has no window to coordinate with.
    type Device = (std::sync::Arc<wgpu::Device>, std::sync::Arc<wgpu::Queue>);

    fn device(&self) -> Option<Self::Device> {
        let context = WgpuRenderer::gpu_context(self)?;
        let context = context.borrow();
        let context = context.as_ref()?;
        Some((context.device.clone(), context.queue.clone()))
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
