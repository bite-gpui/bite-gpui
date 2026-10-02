//! Linux-specific extensions to [`Window`].
//!
//! Window APIs that only make sense on one platform live beside it rather than
//! on the cross-platform type, so that the base surface names no operating
//! system types. Import the extension trait to reach them.

use crate::{Bounds, Pixels, Window};
use gpui_engine::{DmaBufHandle, SurfaceSource};

/// Linux-specific drawing on a [`Window`].
pub trait LinuxWindowExt {
    /// Paint a dma-buf surface into the scene for the next frame at the current z-index.
    ///
    /// The descriptor carries its own layout — a descriptor per plane, an offset, a stride and a
    /// modifier — so the renderer imports it rather than assuming one. The producer keeps the
    /// buffer's contract ([`DmaBufHandle`]): it is uncompressed under its modifier, linear across a
    /// vendor boundary, and a dedicated allocation when its planes are split. This method should
    /// only be called as part of the paint phase of element drawing.
    fn paint_surface(&mut self, bounds: Bounds<Pixels>, handle: DmaBufHandle);
}

impl LinuxWindowExt for Window<'_> {
    fn paint_surface(&mut self, bounds: Bounds<Pixels>, handle: DmaBufHandle) {
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
                source: SurfaceSource::DmaBuf(handle),
            });
    }
}
