//! `GpuCanvas`: the authoring surface for pixels produced outside GPUI.
//!
//! A map, a viewport, a video frame, a 3D scene — content a producer renders offscreen and hands
//! to the window — composites through this element. It is an ordinary box (it *is* a [`Div`]) with
//! a GPU surface painted after the box's own background and border, so the content stacks like any
//! other layer.

use crate::{
    App, Bounds, Div, Element, ElementId, GlobalElementId, InteractiveElement, Interactivity,
    IntoElement, LayoutId, ParentElement, Pixels, StyleRefinement, Styled, Window, div,
};
#[cfg(not(target_os = "windows"))]
use crate::{Corners, ImportedTextureHandle};

/// Build a GPU canvas. Supply the content with `on_render_surface` (a `SurfaceSource`, on macOS or
/// Windows) or `on_render_texture` (a same-device texture handle, elsewhere).
pub fn gpu_canvas() -> GpuCanvas {
    GpuCanvas {
        div: div(),
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        on_render_surface: None,
        #[cfg(not(target_os = "windows"))]
        on_render_texture: None,
    }
}

/// An element that composites a surface produced outside GPUI, on top of an ordinary box.
///
/// All the layout, styling and interactivity are a [`Div`]'s — this forwards every trait to the
/// box it holds, so a canvas behaves like any other element — and only the paint phase adds the
/// one thing a `Div` cannot: a surface primitive pushed after the box.
pub struct GpuCanvas {
    div: Div,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    on_render_surface: Option<
        Box<
            dyn FnOnce(Bounds<Pixels>, &mut Window, &mut App) -> Option<gpui_engine::SurfaceSource>,
        >,
    >,
    #[cfg(not(target_os = "windows"))]
    on_render_texture: Option<
        Box<dyn FnOnce(Bounds<Pixels>, &mut Window, &mut App) -> Option<ImportedTextureHandle>>,
    >,
}

impl GpuCanvas {
    /// Supply the content: a callback, run at paint time, that produces a [`SurfaceSource`]
    /// on the window's renderer's device, or `None` to paint no surface this frame.
    ///
    /// The callback is `FnOnce` and consumed once, matching [`crate::Canvas`]: the element tree is
    /// rebuilt every frame, so a mode is used exactly once. Paint time is when the window's
    /// renderer — and so its device — is guaranteed to exist.
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    pub fn on_render_surface(
        mut self,
        on_render: impl 'static
        + FnOnce(
            Bounds<Pixels>,
            &mut Window,
            &mut App,
        ) -> Option<gpui_engine::SurfaceSource>,
    ) -> Self {
        self.on_render_surface = Some(Box::new(on_render));
        self
    }

    /// Supply the content: a callback, run at paint time, that produces a same-device
    /// [`ImportedTextureHandle`] — a `wgpu::TextureView` or an `id<MTLTexture>` — or `None` to
    /// paint nothing this frame.
    ///
    /// This is the same-device path, which the surface enum does not cover on macOS and Linux: a
    /// producer that renders on the window's own device hands its texture here rather than through
    /// an `IOSurface` or a dma-buf. The callback is `FnOnce` and consumed once, matching
    /// [`crate::Canvas`].
    #[cfg(not(target_os = "windows"))]
    pub fn on_render_texture(
        mut self,
        on_render: impl 'static
        + FnOnce(Bounds<Pixels>, &mut Window, &mut App) -> Option<ImportedTextureHandle>,
    ) -> Self {
        self.on_render_texture = Some(Box::new(on_render));
        self
    }
}

impl Styled for GpuCanvas {
    fn style(&mut self) -> &mut StyleRefinement {
        self.div.style()
    }
}

impl InteractiveElement for GpuCanvas {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.div.interactivity()
    }
}

impl ParentElement for GpuCanvas {
    fn extend(&mut self, elements: impl IntoIterator<Item = crate::AnyElement>) {
        self.div.extend(elements)
    }
}

impl IntoElement for GpuCanvas {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for GpuCanvas {
    type RequestLayoutState = <Div as Element>::RequestLayoutState;
    type PrepaintState = <Div as Element>::PrepaintState;

    fn id(&self) -> Option<ElementId> {
        <Div as Element>::id(&self.div)
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        self.div.source_location()
    }

    fn a11y_role(&self) -> Option<accesskit::Role> {
        self.div.a11y_role()
    }

    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.div.write_a11y_info(node);
    }

    fn a11y_synthetic_children(
        &mut self,
        prepaint: &mut Self::PrepaintState,
        builder: &mut crate::A11ySubtreeBuilder,
    ) {
        self.div.a11y_synthetic_children(prepaint, builder);
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.div.request_layout(id, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.div.prepaint(id, bounds, request_layout, window, cx)
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        // The box paints first — background, border, children — so the GPU content sits above
        // them and below any later sibling, which is how a control stack over a map works at all.
        self.div
            .paint(id, bounds, request_layout, prepaint, window, cx);

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        if let Some(source) = self
            .on_render_surface
            .take()
            .and_then(|on_render| on_render(bounds, window, cx))
        {
            match source {
                #[cfg(target_os = "macos")]
                gpui_engine::SurfaceSource::CoreVideo(image_buffer) => {
                    use crate::MacWindowExt as _;
                    window.paint_surface(bounds, image_buffer);
                }
                #[cfg(target_os = "windows")]
                gpui_engine::SurfaceSource::DirectX(view) => {
                    use crate::WindowsWindowExt as _;
                    window.paint_surface(bounds, view);
                }
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }

        #[cfg(not(target_os = "windows"))]
        if let Some(handle) = self
            .on_render_texture
            .take()
            .and_then(|on_render| on_render(bounds, window, cx))
        {
            window.paint_imported_texture(
                handle,
                bounds,
                // No rounded corners, opaque, and no y flip: the producer and the renderer are
                // the same device with the same UV convention.
                Corners::default(),
                1.0,
                false,
            );
        }
    }
}
