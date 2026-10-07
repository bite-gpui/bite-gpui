//! Paint a texture produced outside GPUI.
//!
//! ```sh
//! cargo run -p gpui --example path_a
//! ```
//!
//! A window's renderer owns the device a texture has to be made on, so producing one is the window
//! owner's capability: this asks the window for its device, fills an offscreen texture on it every
//! frame, and hands the renderer a token for it. The window composites that texture in the same
//! pass as the rest of the UI — the label below is an ordinary `div()` painted over it, and the two
//! stack.
//!
//! The producer differs per platform because the renderer does:
//!
//! - **Linux** — wgpu, on the `GpuContext` the window's own `WgpuRenderer` draws through. The
//!   texture is made on that device and submitted on that queue, so there is nothing to share and
//!   nothing to signal: submission order is the ordering.
//! - **macOS** — wgpu again, but the renderer is Metal's, so there is no wgpu context to be lent.
//!   wgpu makes its own device, and macOS hands it the `MTLDevice` the `MetalRenderer` already owns,
//!   so the texture it writes is one the renderer can sample on the same device.
//! - **Windows** — Direct3D 11, on the `ID3D11Device` the default `DirectXRenderer` lends. This is
//!   what a Media Foundation decoder or a D3D11 engine would do. A *wgpu* producer on Windows needs
//!   `WgpuRenderer` installed through a window factory instead, because wgpu has no Direct3D 11
//!   backend: it could never be on this renderer's device.
//!
//! What the three have in common, and what the demo leans on, is the colour invariant: the texture
//! holds sRGB-encoded bytes, and every arm hands the composited bytes back unchanged. What differs
//! is how they get there — wgpu and Metal declare an sRGB texture and the sampler decodes it, while
//! Direct3D samples the non-sRGB counterpart of the same resource and writes the sample straight
//! through — which is why the content below is written as bytes rather than as a clear colour the
//! two APIs would interpret differently.

#[cfg(target_family = "wasm")]
fn main() {
    // The producers are a desktop GPU's; there is nothing to demonstrate on the web build.
}

#[cfg(not(target_family = "wasm"))]
fn main() {
    demo::run();
}

#[cfg(not(target_family = "wasm"))]
mod demo {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gpui::gpu_canvas;
    use gpui::{
        App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions, div,
        prelude::*, px, rgb, size,
    };

    /// The producer's texture, which the sampler stretches over the window.
    const TEXTURE: u32 = 256;

    /// What a frame hands the window: a `SurfaceSource` on Windows, an imported-texture handle on
    /// the platforms whose renderer is wgpu or Metal.
    #[cfg(target_os = "windows")]
    type Frame = gpui::SurfaceSource;
    #[cfg(not(target_os = "windows"))]
    type Frame = gpui::ImportedTextureHandle;

    pub fn run() {
        gpui::application().run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(480.), px(480.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| PathA::new()),
            )
            .expect("a window to composite into");
            cx.activate(true);
        });
    }

    struct PathA {
        /// `Rc<RefCell<…>>` rather than a field of its own, because the paint closure the canvas
        /// takes is `'static` and the canvas consumes it every frame.
        producer: Rc<RefCell<Option<Producer>>>,
        started: Instant,
    }

    impl PathA {
        fn new() -> Self {
            Self {
                producer: Rc::new(RefCell::new(None)),
                started: Instant::now(),
            }
        }
    }

    impl Render for PathA {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .bg(rgb(0x101014))
                .child(self.gpu_content())
                .child(
                    // Ordinary GPUI elements over the top: an imported texture is a primitive in the
                    // frame's own pass, not an overlay.
                    div()
                        .absolute()
                        .top_4()
                        .left_4()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(0x000000))
                        .opacity(0.65)
                        .text_color(rgb(0xffffff))
                        .child("A texture produced outside GPUI, composited into this window"),
                )
        }
    }

    impl PathA {
        /// The GPU content, filling the window.
        ///
        /// Every platform composites through `GpuCanvas`. On Windows the producer fills a Direct3D
        /// texture each frame and hands its shader resource view through `on_render_surface` to
        /// `surface()`. On Linux and macOS the producer's same-device texture goes through
        /// `on_render_texture`, which pushes the handle through `paint_imported_texture`.
        #[cfg(target_os = "windows")]
        fn gpu_content(&self) -> impl IntoElement {
            let producer = self.producer.clone();
            let started = self.started;

            gpu_canvas()
                .size_full()
                .on_render_surface(move |_bounds, window, _cx| {
                    let mut slot = producer.borrow_mut();
                    // Built on the first frame rather than at construction: the window's renderer —
                    // and so its device — exists by the time it paints.
                    let producer = slot.get_or_insert_with(|| Producer::new(window));

                    let source = match producer.frame(started.elapsed()) {
                        Ok(source) => Some(source),
                        Err(error) => {
                            log::error!("path_a: {error:#}");
                            None
                        }
                    };

                    window.request_animation_frame();
                    source
                })
        }

        #[cfg(not(target_os = "windows"))]
        fn gpu_content(&self) -> impl IntoElement {
            let producer = self.producer.clone();
            let started = self.started;

            gpu_canvas()
                .size_full()
                .on_render_texture(move |_bounds, window, _cx| {
                    let mut slot = producer.borrow_mut();
                    let producer = slot.get_or_insert_with(|| Producer::new(window));

                    let handle = match producer.frame(started.elapsed()) {
                        Ok(handle) => Some(handle),
                        Err(error) => {
                            log::error!("path_a: {error:#}");
                            None
                        }
                    };

                    window.request_animation_frame();
                    handle
                })
        }
    }

    /// The producer, per platform.
    enum Producer {
        /// This should suppport all platforms
        /// When it doesn't, drop into platform specific producer
        #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
        Wgpu(wgpu_producer::WgpuProducer),
        /// DirectX 11 is the only source/target Wgpu doesn't support.
        /// This allows to sample pixels from both D3D11 (same device texture) and D3D12 (shared view)
        #[cfg(target_os = "windows")]
        DirectX(directx_producer::DirectXProducer),
        /// No device to produce on. Stored rather than returned, so the reason is logged once
        /// instead of once per frame.
        Unavailable(String),
    }

    impl Producer {
        fn new(window: &Window) -> Self {
            match Self::try_new(window) {
                Ok(producer) => producer,
                Err(error) => {
                    log::error!("path_a: cannot produce a texture: {error:#}");
                    Self::Unavailable(format!("{error:#}"))
                }
            }
        }

        fn try_new(window: &Window) -> anyhow::Result<Self> {
            #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
            let producer = Self::Wgpu(wgpu_producer::WgpuProducer::new(window)?);
            #[cfg(target_os = "windows")]
            let producer = Self::DirectX(directx_producer::DirectXProducer::new(window)?);
            Ok(producer)
        }

        /// Fills the texture for this frame and returns what the renderer samples.
        fn frame(&mut self, elapsed: Duration) -> anyhow::Result<Frame> {
            match self {
                #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
                Self::Wgpu(producer) => producer.frame(elapsed),
                #[cfg(target_os = "windows")]
                Self::DirectX(producer) => producer.frame(elapsed),
                Self::Unavailable(reason) => anyhow::bail!("{reason}"),
            }
        }
    }

    /// The producer's content, computed on the CPU so the demo needs no shader: a gradient that
    /// drifts in blue, with a white square in the texture's top-left corner so a flipped or rotated
    /// composite would be obvious.
    ///
    /// The bytes are BGRA, the order both targets are in, and they are sRGB-encoded, which is the
    /// invariant: the renderer hands them back unchanged rather than applying a transfer function.
    fn content(size: u32, phase: f32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((size * size * 4) as usize);
        let last = size - 1;
        for y in 0..size {
            for x in 0..size {
                let marker = x < size / 8 && y < size / 8;
                let (r, g, b) = if marker {
                    (0xff, 0xff, 0xff)
                } else {
                    (
                        (0xff * x / last) as u8,
                        (0xff * y / last) as u8,
                        (phase * 255.0) as u8,
                    )
                };
                pixels.extend_from_slice(&[b, g, r, 0xff]);
            }
        }
        pixels
    }

    /// The producer's phase, a slow triangle between 0 and 1.
    fn phase(elapsed: Duration) -> f32 {
        let seconds = elapsed.as_secs_f32();
        (seconds * 0.5).sin() * 0.5 + 0.5
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "macos"))]
    mod wgpu_producer {
        use std::sync::Arc;
        use std::time::Duration;

        use super::{TEXTURE, content, phase};
        use anyhow::Context as _;
        use gpui::{ImportedTextureHandle, Window};

        pub struct WgpuProducer {
            /// Held so the device outlives the texture made on it. It is the same device the window's
            /// renderer draws on, which is the whole of the constraint.
            _device: Arc<wgpu::Device>,
            queue: Arc<wgpu::Queue>,
            texture: wgpu::Texture,
        }

        impl WgpuProducer {
            pub fn new(window: &Window) -> anyhow::Result<Self> {
                let (device, queue) = device(window)?;
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("path_a_producer"),
                    size: wgpu::Extent3d {
                        width: TEXTURE,
                        height: TEXTURE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // sRGB, because the renderer decodes the sample and re-encodes it into a
                    // non-sRGB target: bytes that are already sRGB-encoded come back unchanged.
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    // `TEXTURE_BINDING` is what the token builder checks, and `COPY_DST` is how the
                    // content gets in.
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                Ok(Self {
                    _device: device,
                    queue,
                    texture,
                })
            }

            pub fn frame(&mut self, elapsed: Duration) -> anyhow::Result<ImportedTextureHandle> {
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &self.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &content(TEXTURE, phase(elapsed)),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(TEXTURE * 4),
                        rows_per_image: Some(TEXTURE),
                    },
                    wgpu::Extent3d {
                        width: TEXTURE,
                        height: TEXTURE,
                        depth_or_array_layers: 1,
                    },
                );

                // Nothing to signal: the renderer submits on this same queue and composites after
                // this write, which is the same-device, one-queue case.
                imported(&self.texture)
            }
        }

        /// Linux's account is the one with the device in it: the window's renderer is wgpu's, and
        /// the shared slot is what it draws through.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn device(window: &Window) -> anyhow::Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
            let slot = window
                .device_any()
                .and_then(|any| any.downcast::<gpui_wgpu::GpuContext>().ok())
                .context("the window's renderer lends a wgpu context")?;
            let context = slot.borrow();
            let context = context
                .as_ref()
                .context("the shared context slot is empty")?;
            Ok((context.device.clone(), context.queue.clone()))
        }

        /// macOS's renderer is Metal's, so there is no wgpu context to be *given*; wgpu creates its
        /// own, and macOS hands out the one device to every creator — including GPUI — so this
        /// lands on the renderer's `MTLDevice` rather than beside it.
        #[cfg(target_os = "macos")]
        fn device(_window: &Window) -> anyhow::Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
            use anyhow::anyhow;

            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::METAL,
                flags: wgpu::InstanceFlags::default(),
                backend_options: wgpu::BackendOptions::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                display: None,
            });
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    // Any preference lands on the one device, but naming the low-power one matches what
                    // `MetalRenderer::create_device` picks on a machine that has two.
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                }))
                .map_err(|error| anyhow!("no Metal adapter: {error}"))?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("path_a_producer"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                }))
                .map_err(|error| anyhow!("no wgpu device: {error}"))?;
            Ok((Arc::new(device), Arc::new(queue)))
        }

        /// Where wgpu is the window's own renderer, the token is a view of the texture.
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        fn imported(texture: &wgpu::Texture) -> anyhow::Result<ImportedTextureHandle> {
            use gpui::ImportedTextureExt as _;

            texture
                .create_view(&wgpu::TextureViewDescriptor::default())
                .to_imported_handle()
        }

        /// Where the consumer is Metal's, the token is the texture's own `id<MTLTexture>`: the
        /// renderer resolves it with the `metal` crate, the API `gpui_apple` speaks.
        #[cfg(target_os = "macos")]
        fn imported(texture: &wgpu::Texture) -> anyhow::Result<ImportedTextureHandle> {
            use foreign_types::ForeignTypeRef as _;
            use gpui::MetalTextureExt as _;

            unsafe {
                let hal = texture
                    .as_hal::<wgpu::hal::api::Metal>()
                    .context("the texture has no Metal handle")?;
                let texture = metal::TextureRef::from_ptr(hal.raw_handle() as *const _ as *mut _);
                texture.to_imported_handle()
            }
        }
    }

    /// Windows' producer: Direct3D 11 on the device the default renderer lends, which is the route
    /// a hardware decoder takes.
    #[cfg(target_os = "windows")]
    mod directx_producer {
        use std::time::Duration;

        use super::{TEXTURE, content, phase};
        use anyhow::Context as _;
        use gpui::{SurfaceSource, Window};
        use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC,
            D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV,
            D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11DeviceContext,
            ID3D11ShaderResourceView, ID3D11Texture2D,
        };
        use windows::Win32::Graphics::Dxgi::Common::{
            DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
        };

        pub struct DirectXProducer {
            context: ID3D11DeviceContext,
            texture: ID3D11Texture2D,
            view: ID3D11ShaderResourceView,
        }

        impl DirectXProducer {
            pub fn new(window: &Window) -> anyhow::Result<Self> {
                let device = window
                    .device_any()
                    .and_then(|any| any.downcast::<ID3D11Device>().ok())
                    .context("the window's renderer lends an ID3D11Device")?;

                // The format the sampler requires: the renderer's target is `B8G8R8A8_UNORM`, so
                // a texture in anything else would composite with its channels reordered rather
                // than failing.
                let desc = D3D11_TEXTURE2D_DESC {
                    Width: TEXTURE,
                    Height: TEXTURE,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: (D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE).0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                };
                let mut texture = None;
                unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
                let texture = texture.context("CreateTexture2D returned no texture")?;

                // The view the `surface()` element carries: a non-sRGB B8G8R8A8 view of the same
                // texture, so the renderer samples the producer's bytes straight through.
                let view = unsafe {
                    let desc = D3D11_SHADER_RESOURCE_VIEW_DESC {
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        ViewDimension: D3D_SRV_DIMENSION_TEXTURE2D,
                        Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                            Texture2D: D3D11_TEX2D_SRV {
                                MostDetailedMip: 0,
                                MipLevels: 1,
                            },
                        },
                    };
                    let mut view = None;
                    device.CreateShaderResourceView(&texture, Some(&desc), Some(&mut view))?;
                    view.context("CreateShaderResourceView returned no view")?
                };

                // An upload is recorded on the immediate context, so it is applied before the
                // renderer draws the frame that samples this — nothing to submit and nothing to
                // wait on, which is the same one-queue ordering Linux has.
                let context = unsafe { device.GetImmediateContext() }
                    .context("the device has no immediate context")?;

                Ok(Self {
                    context,
                    texture,
                    view,
                })
            }

            pub fn frame(&mut self, elapsed: Duration) -> anyhow::Result<SurfaceSource> {
                let pixels = content(TEXTURE, phase(elapsed));
                unsafe {
                    self.context.UpdateSubresource(
                        &self.texture,
                        0,
                        None,
                        pixels.as_ptr() as _,
                        TEXTURE * 4,
                        0,
                    );
                }

                Ok(self.view.clone().into())
            }
        }
    }
}
