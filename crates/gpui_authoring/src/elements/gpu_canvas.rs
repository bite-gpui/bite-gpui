//! `GpuCanvas`: the authoring surface for pixels produced outside GPUI.
//!
//! A map, a viewport, a video frame, a 3D scene — content a producer renders offscreen and hands
//! to the window — composites through this element. It is an ordinary box (it *is* a [`Div`]) with
//! a GPU surface painted after the box's own background and border, so the content stacks like any
//! other layer.

use crate::{
    App, Bounds, Corners, Div, Element, ElementId, GlobalElementId, GpuRenderer,
    ImportedTextureHandle, InteractiveElement, Interactivity, IntoElement, LayoutId, PaintSurface,
    ParentElement, Pixels, StyleRefinement, Styled, SurfaceSource, Window, div,
};

/// Build a GPU canvas, an ordinary box whose content a paint-time callback supplies.
///
/// The callback receives a [`GpuCanvasContext`], the only place raw GPU import is reachable: call
/// [`GpuCanvasContext::paint_surface`] to composite a surface produced outside GPUI, or
/// [`GpuCanvasContext::paint_texture`] for a texture made on the window renderer's own device. The
/// callback is `FnOnce` and consumed once, matching [`crate::Canvas`]: the element tree is rebuilt
/// every frame, so it is used exactly once. Paint time is when the window's renderer — and so its
/// device — is guaranteed to exist.
pub fn gpu_canvas(content: impl 'static + FnOnce(&mut GpuCanvasContext)) -> GpuCanvas {
    GpuCanvas {
        div: div(),
        content: Some(Box::new(content)),
    }
}

/// A slice of `Window` — the only place raw GPU import is reachable.
///
/// Handed to a `gpu_canvas` callback. A plain `&mut Window` or an `App` cannot composite a foreign
/// surface; a canvas narrows the window to exactly the operations that can. Bounded by the canvas,
/// so `paint_surface`/`paint_texture` composite into its bounds.
pub struct GpuCanvasContext<'a, 'b> {
    window: &'a mut Window<'b>,
    cx: &'a mut App,
    bounds: Bounds<Pixels>,
}

impl GpuCanvasContext<'_, '_> {
    /// Composite `source` (a dma-buf, a Direct3D texture/view, or a CoreVideo buffer) into the
    /// canvas's bounds for this frame.
    pub fn paint_surface(&mut self, source: impl Into<SurfaceSource>) {
        self.window.core.invalidator.debug_assert_paint();

        let bounds = self.window.snap_bounds(self.bounds);
        let content_mask = self.window.snapped_content_mask();
        self.window
            .frame_state
            .next_frame
            .scene
            .insert_primitive(PaintSurface {
                order: 0,
                bounds,
                content_mask,
                source: source.into(),
            });
    }

    /// Composite a texture produced on the window renderer's own device into the canvas's bounds.
    pub fn paint_texture(
        &mut self,
        handle: ImportedTextureHandle,
        corner_radii: Corners<Pixels>,
        opacity: f32,
        flip_v: bool,
    ) {
        self.window
            .paint_imported_texture(handle, self.bounds, corner_radii, opacity, flip_v);
    }

    /// The canvas's bounds.
    pub fn bounds(&self) -> Bounds<Pixels> {
        self.bounds
    }

    /// The window renderer's device, downcast to `R`.
    ///
    /// This is the same-device producer's door: it hands back an owned handle (`ID3D11Device`, a
    /// `metal::Device`, a wgpu device and queue) that the caller may keep, because a borrow of the
    /// renderer's device cannot outlive the call. Naming `R` is the assertion that the window draws
    /// through that backend — a `#[cfg]`-selected producer already knows — so a mismatch panics; use
    /// [`try_device`](Self::try_device) to handle it instead.
    pub fn device<R: GpuRenderer>(&mut self) -> R::Device {
        self.try_device::<R>().unwrap_or_else(|| {
            panic!(
                "the window's renderer is not a {}: use try_device to handle this",
                std::any::type_name::<R>(),
            )
        })
    }

    /// The window renderer's device, downcast to `R`, or `None` when the renderer is not `R` or has
    /// no device to lend right now.
    ///
    /// The fallible form of [`device`](Self::device), for a producer that degrades — paints a
    /// dma-buf instead of a same-device texture, or skips the frame — rather than asserting the
    /// backend it is running under.
    pub fn try_device<R: GpuRenderer>(&mut self) -> Option<R::Device> {
        let mut lent = None;
        self.window.core.platform_window.with_renderer(&mut |renderer| {
            lent = renderer.as_renderer::<R>().and_then(GpuRenderer::device);
        });
        lent
    }

    /// The application, mirroring the `cx` a `canvas()` callback receives.
    pub fn cx(&mut self) -> &mut App {
        self.cx
    }
}

impl<'a, 'b> GpuCanvasContext<'a, 'b> {
    pub(crate) fn new(
        window: &'a mut Window<'b>,
        cx: &'a mut App,
        bounds: Bounds<Pixels>,
    ) -> Self {
        Self {
            window,
            cx,
            bounds,
        }
    }
}

/// An element that composites a surface produced outside GPUI, on top of an ordinary box.
///
/// All the layout, styling and interactivity are a [`Div`]'s — this forwards every trait to the
/// box it holds, so a canvas behaves like any other element — and only the paint phase adds the
/// one thing a `Div` cannot: a surface primitive pushed after the box.
pub struct GpuCanvas {
    div: Div,
    content: Option<Box<dyn FnOnce(&mut GpuCanvasContext)>>,
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

        if let Some(content) = self.content.take() {
            content(&mut GpuCanvasContext::new(window, cx, bounds));
        }
    }
}
