//! The renderer contract that engines implement.

use crate::{PlatformAtlas, Scene};
use gpui_types::{DevicePixels, Size};
use std::any::Any;
use std::sync::Arc;

/// Downcasting for [`SceneRenderer`] trait objects.
///
/// `SceneRenderer` is the GPU-agnostic seam: it presents a [`Scene`] and names no graphics API.
/// The renderer *behind* it is what owns the device — and the caches, the textures and the other
/// resources built on it — so a caller that needs those downcasts to the concrete backend renderer
/// here, rather than having the window lend its guest's internals.
impl dyn SceneRenderer + '_ {
    /// `self` as the concrete renderer `T`, if that is what it is.
    pub fn as_renderer<T: Any>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref::<T>()
    }

    /// The mutable form of [`as_renderer`](Self::as_renderer).
    pub fn as_renderer_mut<T: Any>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut::<T>()
    }
}

/// A frame read back from a renderer, as raw pixels.
///
/// This is the engine's own type rather than an `image::RgbaImage` because the contract is what
/// every renderer implements, and an image codec crate is not something a renderer needs: the
/// consumers that want one — a PNG in test support, a fixture diff, an export — sit above the
/// engine and convert. `image` was a dependency of this crate for this signature alone.
///
/// The layout is fixed so that the boundary needs no negotiation: RGBA8, unpremultiplied,
/// tightly packed, row-major, top-left origin. A backend whose target is BGRA or premultiplied
/// converts while it copies, which is where the swizzle already happens; a backend with other
/// pixel formats widens this type rather than smuggling a second layout through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PixelBuffer {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl PixelBuffer {
    /// Wraps `data` as an image of `width` by `height` pixels.
    ///
    /// Fails if `data` is not exactly `width * height * 4` bytes, so that the accessors below
    /// cannot hand out a buffer shorter than the size they report.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> anyhow::Result<Self> {
        let expected = width as usize * height as usize * 4;
        anyhow::ensure!(
            data.len() == expected,
            "a {width}x{height} RGBA8 image is {expected} bytes, but the buffer is {}",
            data.len()
        );
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// The width, in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height, in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, RGBA8 and tightly packed.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Consumes the buffer, returning its pixels.
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

/// A renderer that presents a [`Scene`] to some target.
///
/// OS backends and alternative engines implement this; `gpui` produces the
/// [`Scene`] and submits it through the window's platform implementation.
///
/// Offscreen rendering is part of this contract rather than a testing shim: a renderer that is
/// never given a window is how server-side rendering, headless capture and snapshotting are
/// expressed, and the window's renderer is the only thing that owns the device. Onscreen
/// renderers therefore only *need* [`draw`](Self::draw) and [`sprite_atlas`](Self::sprite_atlas);
/// the rest have defaults that report themselves unsupported.
///
/// Rendering and readback are separate steps on purpose. [`render_scene`](Self::render_scene)
/// fills a target without a CPU round trip, which is what a consumer on the GPU wants;
/// [`read_pixels`](Self::read_pixels) pays for system memory only when a consumer on the CPU
/// needs it; [`render_scene_to_image`](Self::render_scene_to_image) is the two together.
pub trait SceneRenderer: Any {
    /// Encodes and submits `scene`, returning whether it was presented.
    ///
    /// Renderers that cannot observe presentation (or that render offscreen)
    /// return `true`; window backends that pace themselves on compositor
    /// callbacks use `false` to retry the frame later.
    fn draw(&mut self, scene: &Scene) -> bool;

    /// Returns the sprite atlas used by this renderer.
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas>;

    /// Sets the size of the target that [`draw`](Self::draw) renders into.
    ///
    /// Onscreen renderers derive their target from the window and can ignore
    /// this; headless renderers use it to size an offscreen texture.
    fn set_viewport_size(&mut self, _size: Size<DevicePixels>) {}

    /// Renders `scene` into an offscreen target of `size`, without reading it back.
    ///
    /// This is the headless analogue of presenting a frame: it performs the same CPU-side
    /// scene encoding and GPU submission as drawing to a real window, but does not block on
    /// GPU completion or copy pixels back.
    fn render_scene(&mut self, _scene: &Scene, _size: Size<DevicePixels>) -> anyhow::Result<()> {
        anyhow::bail!("offscreen rendering is not supported by this renderer")
    }

    /// Reads the offscreen target of the last [`render_scene`](Self::render_scene) back.
    ///
    /// Only a renderer that has rendered offscreen can answer this; the failure is the same
    /// "unsupported" one, one step later.
    fn read_pixels(&mut self) -> anyhow::Result<PixelBuffer> {
        anyhow::bail!("pixel readback is not supported by this renderer")
    }

    /// Renders `scene` offscreen and reads the result back.
    fn render_scene_to_image(
        &mut self,
        scene: &Scene,
        size: Size<DevicePixels>,
    ) -> anyhow::Result<PixelBuffer> {
        self.render_scene(scene, size)?;
        self.read_pixels()
    }
}

/// A renderer that hands every scene it is given to a closure before passing it on.
///
/// Observation belongs here rather than in a backend. It needs nothing from the platform, so a
/// backend that learned about inspection would carry it once per API, and the *scene* is already
/// an engine type: a scene optimiser, a culling pass, an assertion about what primitives a
/// renderer is handed and in what order, are all statements about this layer.
///
/// What a window painted is observable one layer up, where the frame still holds the scene it
/// built (`Window::painted_quads`), so this is for observing a renderer rather than a frame.
///
/// This wraps a renderer in another renderer, and the choices that make it useful are the
/// forwarding ones: it observes [`draw`](SceneRenderer::draw) and
/// [`render_scene`](SceneRenderer::render_scene), and it does *not* override
/// [`render_scene_to_image`](SceneRenderer::render_scene_to_image), so a readback is observed
/// once through the `render_scene` it composes rather than twice.
pub struct ObservingRenderer<R, F> {
    inner: R,
    on_scene: F,
}

impl<R, F> ObservingRenderer<R, F> {
    /// Wraps `inner`, calling `on_scene` with each scene it is asked to draw.
    pub fn new(inner: R, on_scene: F) -> Self {
        Self { inner, on_scene }
    }

    /// The renderer being observed.
    pub fn inner(&self) -> &R {
        &self.inner
    }

    /// The renderer being observed.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Takes back the observed renderer.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R, F> SceneRenderer for ObservingRenderer<R, F>
where
    R: SceneRenderer,
    F: FnMut(&Scene) + 'static,
{
    fn draw(&mut self, scene: &Scene) -> bool {
        (self.on_scene)(scene);
        self.inner.draw(scene)
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.inner.sprite_atlas()
    }

    fn set_viewport_size(&mut self, size: Size<DevicePixels>) {
        self.inner.set_viewport_size(size)
    }

    fn render_scene(&mut self, scene: &Scene, size: Size<DevicePixels>) -> anyhow::Result<()> {
        (self.on_scene)(scene);
        self.inner.render_scene(scene, size)
    }

    fn read_pixels(&mut self) -> anyhow::Result<PixelBuffer> {
        self.inner.read_pixels()
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, cell::RefCell, rc::Rc};

    use super::*;
    use crate::{AtlasKey, AtlasTile, CustomRenderPrimitive, ImportedTextureHandle};
    use gpui_types::{Bounds, ContentMask, Corners};

    /// The observer never uploads, so its atlas only has to exist.
    struct StubAtlas;

    impl PlatformAtlas for StubAtlas {
        fn get_or_insert_with<'a>(
            &self,
            _key: AtlasKey,
            _build: &mut dyn FnMut() -> anyhow::Result<Option<(Size<DevicePixels>, Cow<'a, [u8]>)>>,
        ) -> anyhow::Result<Option<AtlasTile>> {
            Ok(None)
        }

        fn remove(&self, _key: &AtlasKey) {}
    }

    struct RecordingRenderer {
        draws: Rc<RefCell<usize>>,
    }

    impl SceneRenderer for RecordingRenderer {
        fn draw(&mut self, _scene: &Scene) -> bool {
            *self.draws.borrow_mut() += 1;
            true
        }

        fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
            Arc::new(StubAtlas)
        }
    }

    /// What the wrap is for: the closure sees the scene the caller handed over, and the render it
    /// forwards to still happens. An observer that swallowed the scene would pass the count and
    /// fail the second assertion, and one that passed the scene on without observing would fail
    /// the first.
    #[test]
    fn an_observing_renderer_observes_the_scene_and_forwards_it() {
        let draws = Rc::new(RefCell::new(0));
        let observed = Rc::new(RefCell::new(Vec::new()));
        let mut renderer = ObservingRenderer::new(
            RecordingRenderer {
                draws: Rc::clone(&draws),
            },
            {
                let observed = Rc::clone(&observed);
                move |scene: &Scene| observed.borrow_mut().push(scene.custom.len())
            },
        );

        let mut scene = Scene::default();
        scene.custom.push(CustomRenderPrimitive::Texture {
            order: 0,
            handle: ImportedTextureHandle::new(()),
            bounds: Bounds::default(),
            content_mask: ContentMask::default(),
            radii: Corners::default(),
            opacity: 1.0,
            flip_v: false,
        });

        assert!(renderer.draw(&scene));

        assert_eq!(*observed.borrow(), [1]);
        assert_eq!(*draws.borrow(), 1);
    }
}
