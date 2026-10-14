//! `GpuCanvas`: the authoring surface for pixels produced outside GPUI.
//!
//! A map, a viewport, a video frame, a 3D scene — content a producer renders offscreen and hands
//! to the window — composites through this element. It is an ordinary box (it *is* a [`Div`]) with
//! a GPU surface painted after the box's own background and border, so the content stacks like any
//! other layer.

use crate::{
    App, Bounds, Corners, Div, Element, ElementId, GlobalElementId, GpuRenderer, HitboxBehavior,
    HitboxFilter, ImportedTextureHandle, InteractiveElement, Interactivity, IntoElement, LayoutId,
    PaintSurface, ParentElement, Pixels, Point, StyleRefinement, Styled, SurfaceSource, Window,
    div,
};
use std::rc::Rc;

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
        covers: None,
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
    /// Where the surface's content is, so the canvas claims the pointer only there. `None` claims
    /// nothing, which is what an ordinary box does.
    covers: Option<Rc<dyn Fn(Point<f32>) -> bool>>,
}

impl GpuCanvas {
    /// Claim the pointer only where the surface's content is, instead of its whole rectangle.
    ///
    /// `covers` is asked with the pointer in the canvas's own coordinates — `0..1` across its bounds,
    /// origin top-left, the frame the composited surface is indexed by — and answers whether the
    /// content is there. Where it is, the canvas holds the pointer the way
    /// [`InteractiveElement::occlude`] does and nothing behind it is hovered; where it is not, the
    /// canvas is an ordinary box and hit-testing carries on to whatever is drawn behind it.
    ///
    /// It is asked at the position of the event being hit-tested, not at the pointer's position when
    /// a frame was drawn, so the answer neither lags an event nor depends on the window redrawing.
    ///
    /// The default — no `covers` — is the ordinary box: the canvas receives its own events, and so
    /// does anything under it. A fully opaque surface wants [`InteractiveElement::occlude`], which
    /// needs no callback.
    pub fn occlude_where(mut self, covers: impl Fn(Point<f32>) -> bool + 'static) -> Self {
        self.covers = Some(Rc::new(covers));
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
        // Claim the pointer the way `occlude` does, but only where the content is: a blocking hitbox
        // narrowed by a filter, so over the gaps hit-testing carries on to the layer behind. The
        // predicate is asked at the *event's* position — a frame's hitboxes are built once, but a
        // filter is not frozen with them — so the answer neither lags an event nor needs a redraw.
        if let Some(covers) = self.covers.clone() {
            let (origin, size) = (bounds.origin, bounds.size);
            let (width, height) = (size.width.as_f32(), size.height.as_f32());
            let interactivity = self.div.interactivity();
            interactivity.hitbox_behavior = HitboxBehavior::BlockMouse;
            interactivity.hitbox_filter = Some(HitboxFilter::new(move |position| {
                if width <= 0.0 || height <= 0.0 {
                    return false;
                }
                let uv = Point {
                    x: (position.x - origin.x).as_f32() / width,
                    y: (position.y - origin.y).as_f32() / height,
                };
                covers(uv)
            }));
        }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AnyWindowHandle, AppContext as _, Context, InputEvent, Modifiers, MouseButton,
        MouseDownEvent, MouseUpEvent, Render, StatefulInteractiveElement, TestAppContext, point,
        px, size,
    };
    use std::{cell::Cell, rc::Rc};

    struct OccludeWhereTestView {
        clicks: Rc<Cell<usize>>,
        painted: Rc<Cell<Option<Bounds<Pixels>>>>,
    }

    impl Render for OccludeWhereTestView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let clicks = self.clicks.clone();
            let painted = self.painted.clone();
            div()
                .relative()
                .size_full()
                // The control the canvas sits over: an ordinary GPUI button filling the window.
                .child(
                    div()
                        .id("behind")
                        .absolute()
                        .size_full()
                        .on_click(move |_, _, _| clicks.set(clicks.get() + 1)),
                )
                // The canvas over it, claiming the pointer only over its left half.
                .child(
                    gpu_canvas(move |gpu| painted.set(Some(gpu.bounds())))
                        .absolute()
                        .size_full()
                        .occlude_where(|uv| uv.x < 0.5),
                )
        }
    }

    #[gpui::test]
    fn occlude_where_claims_the_pointer_only_over_content(cx: &mut TestAppContext) {
        let clicks = Rc::new(Cell::new(0));
        let painted = Rc::new(Cell::new(None));
        let window = cx.open_window(size(px(200.), px(200.)), {
            let clicks = clicks.clone();
            let painted = painted.clone();
            move |_, _| OccludeWhereTestView { clicks, painted }
        });
        let window = AnyWindowHandle::from(window);

        let draw = |cx: &mut TestAppContext| {
            cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
                .unwrap();
        };
        let click = |cx: &mut TestAppContext, position| {
            cx.update_window(window, |_, window, cx| {
                window.dispatch_event(
                    MouseDownEvent {
                        position,
                        modifiers: Modifiers::default(),
                        button: MouseButton::Left,
                        click_count: 1,
                        first_mouse: false,
                    }
                    .to_platform_input(),
                    cx,
                );
                window.dispatch_event(
                    MouseUpEvent {
                        position,
                        modifiers: Modifiers::default(),
                        button: MouseButton::Left,
                        click_count: 1,
                    }
                    .to_platform_input(),
                    cx,
                );
            })
            .unwrap();
        };

        // One frame, so the canvas has painted and its bounds are known: the predicate is asked in
        // the canvas's own 0..1 frame, so the probe points are derived from those bounds rather than
        // assumed.
        draw(cx);
        let painted = painted.get().expect("the canvas painted");
        let over_content = point(
            painted.origin.x + px(painted.size.width.as_f32() * 0.25),
            painted.origin.y + px(painted.size.height.as_f32() * 0.5),
        );
        let over_nothing = point(
            painted.origin.x + px(painted.size.width.as_f32() * 0.75),
            painted.origin.y + px(painted.size.height.as_f32() * 0.5),
        );

        // No move and no redraw between that frame and these clicks: the filter is asked at each
        // event's own position, so the frame drawn before is enough and the answer cannot lag a move.
        // A control behind is reached only where the canvas declines.
        click(cx, over_content);
        assert_eq!(
            clicks.get(),
            0,
            "a click over the canvas's content must not reach the control behind it"
        );

        click(cx, over_nothing);
        assert_eq!(
            clicks.get(),
            1,
            "a click over the empty half must pass through to the control behind it"
        );
    }
}
