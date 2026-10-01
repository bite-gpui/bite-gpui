//! Windows-specific extensions to [`Window`].
//!
//! Window APIs that only make sense on one platform live beside it rather than
//! on the cross-platform type, so that the base surface names no operating
//! system types. Import the extension trait to reach them.

use crate::{Bounds, Pixels, Window};
use gpui_engine::SurfaceSource;
use windows::Win32::Graphics::Direct3D11::ID3D11ShaderResourceView;

/// Windows-specific drawing on a [`Window`].
pub trait WindowsWindowExt {
    /// Paint a Direct3D 11 surface into the scene for the next frame at the
    /// current z-index.
    ///
    /// The view has to be made on this window's renderer's own device — one
    /// resource with two views of it and nothing to synchronise — which is the
    /// device [`Window::device_any`](crate::Window::device_any) lends. A view
    /// from another device composites nothing rather than the wrong memory: the
    /// renderer's binding is what rejects it. This method should only be called
    /// as part of the paint phase of element drawing.
    fn paint_surface(&mut self, bounds: Bounds<Pixels>, view: ID3D11ShaderResourceView);
}

impl WindowsWindowExt for Window<'_> {
    fn paint_surface(&mut self, bounds: Bounds<Pixels>, view: ID3D11ShaderResourceView) {
        use crate::PaintSurface;

        self.core.invalidator.debug_assert_paint();

        let bounds = self.snap_bounds(bounds);
        let content_mask = self.snapped_content_mask();
        self.frame_state
            .next_frame
            .scene
            .insert_primitive(PaintSurface {
                order: 0,
                bounds,
                content_mask,
                source: SurfaceSource::DirectX(view),
            });
    }
}
