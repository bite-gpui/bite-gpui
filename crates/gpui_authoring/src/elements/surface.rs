#[cfg(target_os = "linux")]
use crate::LinuxWindowExt;
#[cfg(target_os = "macos")]
use crate::MacWindowExt;
#[cfg(target_os = "windows")]
use crate::WindowsWindowExt;
use crate::{
    App, Bounds, Element, ElementId, GlobalElementId, IntoElement, LayoutId, ObjectFit, Pixels,
    Style, StyleRefinement, Styled, Window,
};
// The payload lives in the engine beside `PaintSurface`, the primitive it becomes: `surface()`
// takes one and `draw_surfaces` reads it, and neither the element nor a renderer invents a
// transport of its own.
#[cfg(target_os = "windows")]
use gpui_engine::DirectXSource;
use gpui_engine::SurfaceSource;
use refineable::Refineable;

/// A surface element.
pub struct Surface {
    source: SurfaceSource,
    object_fit: ObjectFit,
    style: StyleRefinement,
}

/// Create a new surface element.
pub fn surface(source: impl Into<SurfaceSource>) -> Surface {
    Surface {
        source: source.into(),
        object_fit: ObjectFit::Contain,
        style: Default::default(),
    }
}

impl Surface {
    /// Set the object fit for the image.
    pub fn object_fit(mut self, object_fit: ObjectFit) -> Self {
        self.object_fit = object_fit;
        self
    }
}

impl Element for Surface {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout_id = window.request_layout(style, [], cx);
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        #[cfg_attr(
            not(any(target_os = "macos", target_os = "windows")),
            allow(unused_variables)
        )]
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        #[cfg_attr(
            not(any(target_os = "macos", target_os = "windows")),
            allow(unused_variables)
        )]
        window: &mut Window,
        _: &mut App,
    ) {
        match &self.source {
            #[cfg(target_os = "macos")]
            SurfaceSource::CoreVideo(image_buffer) => {
                let size = crate::size(
                    image_buffer.get_width().into(),
                    image_buffer.get_height().into(),
                );
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                // TODO: Add support for corner_radii
                window.paint_surface(new_bounds, image_buffer.clone());
            }
            #[cfg(target_os = "windows")]
            SurfaceSource::DirectX(source) => {
                let new_bounds = match directx_source_size(source) {
                    Some(size) => self.object_fit.get_bounds(bounds, size),
                    None => bounds,
                };
                // TODO: Add support for corner_radii
                window.paint_surface(new_bounds, source.clone());
            }
            #[cfg(target_os = "linux")]
            SurfaceSource::DmaBuf(handle) => {
                let size = crate::size(
                    crate::DevicePixels::from(handle.width as i32),
                    crate::DevicePixels::from(handle.height as i32),
                );
                let new_bounds = self.object_fit.get_bounds(bounds, size);
                // TODO: Add support for corner_radii
                window.paint_surface(new_bounds, handle.clone());
            }
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
}

impl IntoElement for Surface {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for Surface {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// The pixel size of the texture behind a Direct3D surface source.
///
/// The Windows arm fits the surface to its bounds exactly as the macOS arm does, and neither a
/// texture nor a view carries a size of its own: the texture is the resource, and the view's size is
/// the resource behind it. A source whose resource is not a texture, or whose query fails, has no
/// size to fit, and the element falls back to its bounds.
#[cfg(target_os = "windows")]
fn directx_source_size(source: &DirectXSource) -> Option<crate::Size<crate::DevicePixels>> {
    use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
    use windows::core::Interface as _;

    unsafe {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        match source {
            DirectXSource::Texture(texture) => texture.GetDesc(&mut desc),
            DirectXSource::View(view) => {
                let resource = view.GetResource().ok()?;
                let texture: ID3D11Texture2D = resource.cast().ok()?;
                texture.GetDesc(&mut desc);
            }
        }
        Some(crate::size(
            crate::DevicePixels::from(desc.Width as i32),
            crate::DevicePixels::from(desc.Height as i32),
        ))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{Context, Render, TestAppContext, Window};
    use gpui_engine::{DmaBufFormat, DmaBufHandle, DmaBufPlane, SurfaceSource};
    use std::os::fd::OwnedFd;

    struct SurfaceView {
        handle: DmaBufHandle,
    }

    impl Render for SurfaceView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            surface(self.handle.clone())
        }
    }

    /// A descriptor without a GPU: any open file is a valid one as far as the element cares.
    fn descriptor() -> OwnedFd {
        std::fs::File::open("/dev/null")
            .expect("/dev/null")
            .into()
    }

    /// The element's whole job is to put the handle into the scene as a surface: it names no
    /// renderer, so this needs no GPU.
    #[gpui::test]
    fn a_surface_handle_reaches_the_scene(cx: &mut TestAppContext) {
        let handle = DmaBufHandle::new(
            4,
            4,
            DmaBufFormat::Rgba8,
            DmaBufHandle::LINEAR,
            [DmaBufPlane::new(descriptor(), 0, 16)],
            None,
        );

        let expected = handle.clone();
        // `add_window_view` draws the frame the way the platform does; drawing by hand from inside
        // a `window.update` would re-borrow the view that closure already holds.
        let (_view, cx) = cx.add_window_view(|_, _| SurfaceView { handle });
        cx.run_until_parked();

        let surfaces = cx.update(|window, _cx| window.painted_surfaces());
        assert_eq!(
            surfaces.len(),
            1,
            "the surface should be in the scene exactly once"
        );
        assert_eq!(surfaces[0].source, SurfaceSource::DmaBuf(expected));
    }
}
