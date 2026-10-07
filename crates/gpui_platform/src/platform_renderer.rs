//! The renderer seam: the trait a window holds, the target a renderer is built against, and
//! the factory the window consults.
//!
//! A window holds a `Box<dyn PlatformRenderer>` rather than a concrete renderer, so an
//! application can install its own through the renderer factory. The lifecycle methods keep
//! the names the backends already call them by, and each platform's native hooks — the macOS
//! layer, the Windows background appearance — are supertraits rather than methods, so a
//! backend reaches its own without a downcast and neither widens `SceneRenderer`.
//!
//! A trait's supertraits cannot be cfg-selected, so [`NativeSceneHooks`] is there to be the one
//! that can: it is this platform's hooks, and nothing implements it by hand.

use std::any::Any;
use std::fmt;
use std::rc::Rc;

use gpui_engine::SceneRenderer;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::{DevicePixels, GpuSpecs, Pixels, Size};

/// The macOS hooks a window needs and no other platform does.
#[cfg(target_os = "macos")]
pub trait MacSceneRenderer: SceneRenderer {
    /// The `CAMetalLayer` this renderer draws through, which the window installs as the
    /// view's backing layer from `-[NSView makeBackingLayer]`.
    fn layer_ptr(&self) -> *mut std::ffi::c_void;

    /// Whether presenting waits for the Core Animation transaction, which a resized window
    /// needs.
    fn set_presents_with_transaction(&mut self, value: bool);
}

/// The Windows hooks a window needs and no other platform does.
#[cfg(target_os = "windows")]
pub trait WinSceneRenderer: SceneRenderer {
    /// The window's background appearance changed.
    fn set_background_appearance(&mut self, appearance: crate::WindowBackgroundAppearance);

    /// The drawable size changed. Windows resizes a swap chain here rather than in
    /// `PlatformRenderer::update_drawable_size`, because the failure is one the window has to act
    /// on: a drawable that cannot be resized means the devices are invalid.
    fn resize(&mut self, size: Size<DevicePixels>) -> anyhow::Result<()>;

    /// The renderer may draw again. A renderer that discards the frames between a device loss
    /// and the forced render that follows it clears that state here; only Direct3D has any.
    fn mark_drawable(&mut self) {}
}

/// The platform's native hooks, as a supertrait every [`PlatformRenderer`] has.
#[cfg(target_os = "macos")]
pub trait NativeSceneHooks: MacSceneRenderer {}
/// The platform's native hooks, as a supertrait every [`PlatformRenderer`] has.
#[cfg(target_os = "windows")]
pub trait NativeSceneHooks: WinSceneRenderer {}
/// The platform's native hooks, as a supertrait every [`PlatformRenderer`] has.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub trait NativeSceneHooks: SceneRenderer {}

#[cfg(target_os = "macos")]
impl<T: MacSceneRenderer + ?Sized> NativeSceneHooks for T {}
#[cfg(target_os = "windows")]
impl<T: WinSceneRenderer + ?Sized> NativeSceneHooks for T {}
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl<T: SceneRenderer + ?Sized> NativeSceneHooks for T {}

/// What a window asks of whatever renders into it.
///
/// `draw` and `sprite_atlas` come from [`SceneRenderer`]; this adds the lifecycle a window
/// drives, which an offscreen renderer has no reason to implement. `update_drawable_size` and
/// `max_texture_size` are the two with no default, because a renderer that cannot answer them
/// cannot be a window's.
pub trait PlatformRenderer: NativeSceneHooks {
    /// The target's size changed, in device pixels.
    fn update_drawable_size(&mut self, size: Size<DevicePixels>);

    /// The largest texture this renderer can allocate, in pixels.
    fn max_texture_size(&self) -> u32;

    /// What GPU this renderer is on, if it can tell.
    fn gpu_specs(&self) -> Option<GpuSpecs>;

    /// The graphics device this renderer draws on, for a producer that has to make a texture on
    /// it.
    ///
    /// This is the same-device rule seen from the other side: the device
    /// belongs to whoever built the window's renderer, and a producer — a decoder, an engine, a
    /// viewport — has to render on *that* one. The return is erased because the shared trait must
    /// not name `ID3D11Device` or `MTLDevice`; the payload is the backend's own type (a
    /// `GpuContext`, a `DirectXDevices`, a device), and the crate that owns it is where a caller
    /// downcasts. It is owned rather than borrowed because a caller reaches a renderer through a
    /// closure ([`PlatformWindow::with_renderer`](crate::PlatformWindow::with_renderer)), which
    /// no borrow can outlive.
    ///
    /// `None` means there is no device to lend: a renderer that draws offscreen, or one a factory
    /// installed that is not the backend's own. That is an answer, not a failure.
    fn device_any(&self) -> Option<Rc<dyn Any>> {
        None
    }

    /// Whether glyphs are rasterized with subpixel antialiasing in BGR order.
    fn set_subpixel_layout(&mut self, _is_bgr: bool) {}

    /// The window's transparency changed.
    fn update_transparency(&mut self, _transparent: bool) {}

    /// Release the renderer's resources, keeping it reconstructible.
    fn destroy(&mut self) {}

    /// Whether the GPU device or its surface has been lost.
    fn device_lost(&self) -> bool {
        false
    }

    /// Whether the renderer has work the frame loop should not wait on.
    fn needs_redraw(&mut self) -> bool {
        false
    }

    /// Rebuild from the target the renderer was built against, after a lost device or
    /// surface.
    #[cfg(not(target_family = "wasm"))]
    fn recover(&mut self, _target: RendererTarget<'_>) -> anyhow::Result<()> {
        anyhow::bail!("renderer does not support recovery")
    }
}

/// What a renderer is built against: the window's handles, its geometry, and whatever the
/// backend needs to add.
///
/// The handles are typed rather than erased, because erasing them would make any renderer
/// that is not the backend's own downcast to the backend's target type — which means
/// depending on the backend crate. The backend's own extras are the one erased field.
pub struct RendererTarget<'a> {
    /// The window's raw handle, which the renderer builds its surface from.
    pub window_handle: Option<RawWindowHandle>,
    /// The display the window is on, for the same.
    pub display_handle: Option<RawDisplayHandle>,
    /// The window's size, in logical pixels.
    pub size: Size<Pixels>,
    /// The ratio between logical pixels and device pixels.
    pub scale_factor: f32,
    /// Whether the window is transparent.
    pub transparent: bool,
    /// Backend-specific extras, for that backend's own renderer only. `Any` is `'static`, so
    /// this must point at owned data; a backend with nothing to add passes `None`.
    pub backend: Option<&'a dyn Any>,
}

/// Builds a window's renderer from the surface the window has already made.
///
/// It runs exactly once per window, after the native window exists and before the first
/// frame, because a renderer binds to a surface and the surface comes from the window. On the
/// recovery path the renderer is handed a [`RendererTarget`] built from the surface it already
/// has, not a new one from here.
pub trait RendererFactory: 'static {
    /// Build the renderer this window will draw through.
    fn create(&self, target: RendererTarget<'_>) -> anyhow::Result<Box<dyn PlatformRenderer>>;
}

/// A shared [`RendererFactory`], which is also `Debug` because the window options it lives in
/// are.
#[derive(Clone)]
pub struct DynRendererFactory(pub Rc<dyn RendererFactory>);

impl fmt::Debug for DynRendererFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DynRendererFactory(..)")
    }
}

/// The spelling for the common case: a closure is a factory.
///
/// It is a newtype rather than a blanket `impl RendererFactory for F where F: Fn(..)`, which
/// would collide with any other blanket implementation the moment one appeared.
pub struct FnRendererFactory<F>(pub F);

impl<F> RendererFactory for FnRendererFactory<F>
where
    F: Fn(RendererTarget<'_>) -> anyhow::Result<Box<dyn PlatformRenderer>> + 'static,
{
    fn create(&self, target: RendererTarget<'_>) -> anyhow::Result<Box<dyn PlatformRenderer>> {
        (self.0)(target)
    }
}
