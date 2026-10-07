//! One window that shows every way to composite pixels GPUI did not draw.
//!
//! ```sh
//! cargo run -p gpui --example surface
//! ```
//!
//! The window is a labelled grid of solid olive-green tiles. Each tile is one combination of an
//! **authoring layer** and a **payload**, and the grid is the point: the same colour reaches the
//! window whether it arrives through the `surface()` element or through a `gpu_canvas()` callback,
//! and whichever payload the host renderer can composite.
//!
//! # The two layers
//!
//! - `surface(source)` is an ordinary element. It lays out and stacks like any other child, and the
//!   renderer samples `source` while it composites the frame.
//! - `gpu_canvas(..)` is a box whose content a paint-time callback supplies: it receives a
//!   `GpuCanvasContext` and either paints a surface source into the canvas, or paints a texture the
//!   producer rendered on the window's own device.
//!
//! # The payloads, by renderer
//!
//! Only the combinations the host renderer supports are built and shown; each is labelled in place.
//!
//! - **wgpu** (Linux): `surface()` carries a dma-buf handle in each of `Bgra8`, `Rgba8` and `Nv12`;
//!   `gpu_canvas(..)` paints a `wgpu` texture view.
//! - **Direct3D 11** (Windows): `surface()` carries a `B8G8R8A8_UNORM` texture, and separately a
//!   shader-resource view the producer made; `gpu_canvas(..)` paints a surface source.
//! - **Metal** (macOS): `surface()` carries a CoreVideo pixel buffer; `gpu_canvas(..)` paints the same
//!   buffer and an `id<MTLTexture>`.
//!
//! # One colour, in each payload's own format
//!
//! Every tile is the same olive green. A single-plane payload stores it directly, in that format's
//! byte order; the two-plane `Nv12` payload stores the luma and chroma bytes the renderer's BT.601
//! conversion maps back to that green, and that conversion is worked out and commented where the
//! bytes are made.
//!
//! The producers are the platform's own: a bare Vulkan device exports the Linux dma-bufs, and the
//! window's device (or one the platform hands every creator) backs the textures. This is a working
//! application, not a sketch.

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn main() {
    eprintln!(
        "surface is a desktop example: it produces a GPU texture or dma-buf and composites it \
         through the window's renderer. Run it on Linux, Windows or macOS."
    );
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
fn main() {
    demo::run();
}

#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
mod demo {
    use gpui::{
        AnyElement, App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions,
        div, prelude::*, px, rgb, size,
    };

    /// Olive green (`#808000`), opaque: the one colour every tile is, whatever payload carries it.
    const OLIVE: [u8; 4] = [128, 128, 0, 255];

    /// The produced surface size, in pixels; each element scales it into its tile.
    const TILE: u32 = 128;

    #[cfg(target_os = "linux")]
    const BACKEND: &str =
        "Linux · wgpu — dma-buf Bgra8, Rgba8 and Nv12, and an imported texture view";
    #[cfg(target_os = "windows")]
    const BACKEND: &str =
        "Windows · Direct3D 11 — a texture payload, a view payload, and a canvas surface";
    #[cfg(target_os = "macos")]
    const BACKEND: &str = "macOS · Metal — a CoreVideo buffer and an imported Metal texture";

    pub fn run() {
        gpui::application().run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(780.), px(680.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Showcase::new()),
            )
            .expect("a window to composite into");
            cx.activate(true);
        });
    }

    /// The window's view. The producers live here so their resources outlive every frame.
    struct Showcase {
        #[cfg(target_os = "linux")]
        producer: Option<wgpu_backend::Producer>,
        #[cfg(target_os = "windows")]
        producer: Option<directx_backend::Producer>,
        #[cfg(target_os = "macos")]
        producer: Option<metal_backend::Producer>,
    }

    impl Showcase {
        fn new() -> Self {
            Self { producer: None }
        }
    }

    impl Render for Showcase {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            #[cfg(target_os = "linux")]
            let tiles = wgpu_backend::tiles(&mut self.producer, window);
            #[cfg(target_os = "windows")]
            let tiles = directx_backend::tiles(&mut self.producer, window);
            #[cfg(target_os = "macos")]
            let tiles = metal_backend::tiles(&mut self.producer, window);

            div()
                .size_full()
                .bg(rgb(0x101014))
                .flex()
                .flex_col()
                .items_center()
                .gap_6()
                .p_8()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_2xl()
                                .text_color(rgb(0xffffff))
                                .child("External surfaces"),
                        )
                        .child(div().text_sm().text_color(rgb(0x9a9aa2)).child(BACKEND)),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap_6()
                        .children(tiles),
                )
        }
    }

    /// A square tile with a caption under it, so each combination is labelled the way it was made.
    fn panel(content: impl IntoElement, caption: impl IntoElement) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .w(px(220.))
            .child(div().w(px(170.)).h(px(170.)).child(content))
            .child(div().text_color(rgb(0xd8d8d8)).text_center().child(caption))
            .into_any_element()
    }

    /// A tile that stands in for one the host could not produce, so the grid still explains itself.
    fn error_panel(message: String) -> AnyElement {
        panel(
            div()
                .size_full()
                .bg(rgb(0x2a1518))
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .child(div().text_color(rgb(0xffaaaa)).text_center().child(message)),
            "unavailable on this host",
        )
    }

    /// The Y, Cb and Cr bytes the renderer's BT.601 surface conversion maps back to `rgb`.
    ///
    /// The surface fragment of a two-plane payload converts with this matrix:
    ///
    /// ```text
    /// R = y + 1.4020*cr - 0.7010
    /// G = y - 0.3441*cb - 0.7141*cr + 0.5291
    /// B = y + 1.7720*cb - 0.8860
    /// ```
    ///
    /// which is BT.601, full range — the same matrix the wgpu and Metal shaders carry. Inverting it
    /// (move the offsets to the source side, then apply the luma/chroma rows of the inverse) gives the
    /// bytes to store for a target colour.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn nv12_from_rgb(rgb: [u8; 4]) -> (u8, u8, u8) {
        let r = f32::from(rgb[0]) / 255.0;
        let g = f32::from(rgb[1]) / 255.0;
        let b = f32::from(rgb[2]) / 255.0;
        let (r, g, b) = (r + 0.7010, g - 0.5291, b + 0.8860);
        let y = 0.299 * r + 0.587 * g + 0.114 * b;
        let cb = -0.168736 * r - 0.331264 * g + 0.5 * b;
        let cr = 0.5 * r - 0.418688 * g - 0.081312 * b;
        (
            (y.clamp(0.0, 1.0) * 255.0).round() as u8,
            (cb.clamp(0.0, 1.0) * 255.0).round() as u8,
            (cr.clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }

    /// `OLIVE` in the byte order a `Bgra8`/`B8G8R8A8` payload holds: blue, green, red, alpha.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    const fn olive_bgra() -> [u8; 4] {
        [OLIVE[2], OLIVE[1], OLIVE[0], OLIVE[3]]
    }

    /// `pixels` copies of `pixel`, row-major.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn solid(pixels: u32, pixel: [u8; 4]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(pixels as usize * 4);
        for _ in 0..pixels {
            bytes.extend_from_slice(&pixel);
        }
        bytes
    }

    /// Linux's producer: a bare Vulkan device exports the dma-bufs, and the window's own wgpu device
    /// backs the imported texture.
    #[cfg(target_os = "linux")]
    mod wgpu_backend {
        use std::any::Any;
        use std::cell::RefCell;
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::rc::Rc;
        use std::sync::Arc;

        use anyhow::{Context as _, Result};
        use ash::vk;
        use gpui::{
            AnyElement, Corners, DmaBufFormat, DmaBufHandle, DmaBufPlane, ImportedTextureHandle,
            ImportedTextureExt as _, Window, gpu_canvas, prelude::*, surface,
        };

        use super::{OLIVE, TILE, error_panel, nv12_from_rgb, olive_bgra, panel, solid};

        pub struct Producer {
            /// The device the dma-bufs are exported from. It must outlive every handle made from it.
            _vulkan: Vulkan,
            /// `surface()` with a dma-buf in each of the three formats.
            bgra: DmaBufHandle,
            rgba: DmaBufHandle,
            nv12: DmaBufHandle,
            /// The imported-texture tile. Built at paint time, when the window's device exists; the
            /// `Rc` is shared with the callback so the tile survives across frames instead of being
            /// rebuilt every paint.
            imported: Rc<RefCell<Option<ImportedTile>>>,
        }

        impl Producer {
            fn new() -> Result<Self> {
                let mut vulkan = Vulkan::new()?;

                // A single plane per format, each cleared to olive in its own byte order.
                let bgra_fd = vulkan.allocate(&solid(TILE * TILE, olive_bgra()))?;
                let bgra = DmaBufHandle::new(
                    TILE,
                    TILE,
                    DmaBufFormat::Bgra8,
                    DmaBufHandle::LINEAR,
                    [DmaBufPlane::new(bgra_fd, 0, TILE * 4)],
                    None,
                );

                let rgba_fd = vulkan.allocate(&solid(TILE * TILE, OLIVE))?;
                let rgba = DmaBufHandle::new(
                    TILE,
                    TILE,
                    DmaBufFormat::Rgba8,
                    DmaBufHandle::LINEAR,
                    [DmaBufPlane::new(rgba_fd, 0, TILE * 4)],
                    None,
                );

                // Two planes in one allocation: full-resolution luma, then half-resolution interleaved
                // chroma. Both planes are written whole, padding included.
                let (y, cb, cr) = nv12_from_rgb(OLIVE);
                let luma_size = TILE * TILE;
                let chroma_pairs = TILE / 2 * TILE / 2;
                let mut content = Vec::with_capacity((luma_size + chroma_pairs * 2) as usize);
                content.resize(luma_size as usize, y);
                for _ in 0..chroma_pairs {
                    content.push(cb);
                    content.push(cr);
                }
                let fd = vulkan.allocate(&content)?;
                let chroma_fd = fd
                    .try_clone()
                    .context("duplicate the dma-buf for the chroma plane")?;
                let nv12 = DmaBufHandle::new(
                    TILE,
                    TILE,
                    DmaBufFormat::Nv12,
                    DmaBufHandle::LINEAR,
                    [
                        DmaBufPlane::new(fd, 0, TILE),
                        DmaBufPlane::new(chroma_fd, luma_size as u64, TILE),
                    ],
                    None,
                );

                Ok(Self {
                    _vulkan: vulkan,
                    bgra,
                    rgba,
                    nv12,
                    imported: Rc::new(RefCell::new(None)),
                })
            }
        }

        /// The imported-texture tile: a wgpu texture the window's own device draws through.
        struct ImportedTile {
            /// Held so the device outlives the view taken from the texture.
            _device: Arc<wgpu::Device>,
            _texture: wgpu::Texture,
            handle: ImportedTextureHandle,
        }

        impl ImportedTile {
            fn new(device_any: Option<Rc<dyn Any>>) -> Result<Self> {
                let (device, queue) = device(device_any)?;
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("surface_showcase"),
                    size: wgpu::Extent3d {
                        width: TILE,
                        height: TILE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // sRGB, matching the renderer's imported-texture path: the sampler decodes to
                    // linear and the fragment re-encodes, so these sRGB-encoded bytes come back
                    // unchanged rather than double-encoded.
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &solid(TILE * TILE, olive_bgra()),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(TILE * 4),
                        rows_per_image: Some(TILE),
                    },
                    wgpu::Extent3d {
                        width: TILE,
                        height: TILE,
                        depth_or_array_layers: 1,
                    },
                );
                let handle = texture
                    .create_view(&wgpu::TextureViewDescriptor::default())
                    .to_imported_handle()?;
                Ok(Self {
                    _device: device,
                    _texture: texture,
                    handle,
                })
            }
        }

        pub fn tiles(slot: &mut Option<Producer>, _window: &mut Window) -> Vec<AnyElement> {
            if slot.is_none() {
                match Producer::new() {
                    Ok(producer) => *slot = Some(producer),
                    Err(error) => {
                        log::error!("surface: cannot produce a dma-buf: {error:#}");
                        return vec![error_panel(format!("dma-buf producer unavailable: {error:#}"))];
                    }
                }
            }
            let producer = slot.as_ref().expect("the producer was just built");
            let imported = producer.imported.clone();

            vec![
                panel(
                    surface(producer.bgra.clone()).size_full(),
                    "surface() · DmaBufFormat::Bgra8",
                ),
                panel(
                    surface(producer.rgba.clone()).size_full(),
                    "surface() · DmaBufFormat::Rgba8",
                ),
                panel(
                    surface(producer.nv12.clone()).size_full(),
                    "surface() · DmaBufFormat::Nv12",
                ),
                panel(
                    gpu_canvas(move |gpu| {
                        let mut tile = imported.borrow_mut();
                        if tile.is_none() {
                            match ImportedTile::new(gpu.device_any()) {
                                Ok(built) => *tile = Some(built),
                                Err(error) => {
                                    log::error!(
                                        "surface: cannot produce the imported texture: {error:#}"
                                    );
                                    return;
                                }
                            }
                        }
                        let handle = tile
                            .as_ref()
                            .expect("the tile was just built")
                            .handle
                            .clone();
                        gpu.paint_texture(handle, Corners::default(), 1.0, false);
                    })
                    .size_full(),
                    "gpu_canvas(..) · wgpu texture view",
                ),
            ]
        }

        /// The wgpu device and queue the window's renderer draws through.
        fn device(
            device_any: Option<Rc<dyn Any>>,
        ) -> Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
            let slot = device_any
                .and_then(|any| any.downcast::<gpui_wgpu::WgpuContextSlot>().ok())
                .context("the window's renderer lends a wgpu context")?;
            let context = slot.borrow();
            let context = context.as_ref().context("the shared context slot is empty")?;
            Ok((context.device.clone(), context.queue.clone()))
        }

        /// A minimal Vulkan "producer": the loader, instance and device that keep the exported
        /// dma-bufs alive for as long as the renderer samples them.
        ///
        /// The `Entry` must outlive the device it made — dropping it unloads the loader the device's
        /// calls go through — so it is a field and the device is destroyed explicitly first.
        struct Vulkan {
            _entry: ash::Entry,
            instance: ash::Instance,
            device: ash::Device,
            external_memory_fd: ash::khr::external_memory_fd::Device,
            /// Every allocation ever exported; kept alive, not read.
            memories: Vec<vk::DeviceMemory>,
            /// A host-visible, host-coherent memory type the allocations are made in.
            memory_type_index: u32,
        }

        impl Vulkan {
            fn new() -> Result<Self> {
                let entry = unsafe { ash::Entry::load() }
                    .map_err(|error| anyhow::anyhow!("load vulkan: {error}"))?;
                let app_info =
                    vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
                let instance = unsafe {
                    entry.create_instance(
                        &vk::InstanceCreateInfo::default().application_info(&app_info),
                        None,
                    )
                }
                .context("create the producer instance")?;

                let devices = unsafe { instance.enumerate_physical_devices() }
                    .context("enumerate physical devices")?;
                // Prefer the integrated GPU, the one a renderer is usually steered to; fall back to
                // whatever the driver lists first.
                let physical = devices
                    .iter()
                    .copied()
                    .find(|device| {
                        let properties = unsafe { instance.get_physical_device_properties(*device) };
                        properties.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU
                    })
                    .or_else(|| devices.first().copied())
                    .context("no Vulkan device for the producer")?;

                let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
                let memory_type_index = properties
                    .memory_types
                    .iter()
                    .enumerate()
                    .find(|(_, memory_type)| {
                        memory_type.property_flags.contains(
                            vk::MemoryPropertyFlags::HOST_VISIBLE
                                | vk::MemoryPropertyFlags::HOST_COHERENT,
                        )
                    })
                    .map(|(index, _)| index as u32)
                    .context("no host-visible memory for the producer")?;

                let extensions = [
                    vk::KHR_EXTERNAL_MEMORY_FD_NAME.as_ptr(),
                    vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME.as_ptr(),
                ];
                let device = unsafe {
                    instance.create_device(
                        physical,
                        &vk::DeviceCreateInfo::default().enabled_extension_names(&extensions),
                        None,
                    )
                }
                .context("create the producer device")?;
                let external_memory_fd =
                    ash::khr::external_memory_fd::Device::new(&instance, &device);

                Ok(Self {
                    _entry: entry,
                    instance,
                    device,
                    external_memory_fd,
                    memories: Vec::new(),
                    memory_type_index,
                })
            }

            /// Allocate `content` in a host-visible buffer and export it as a linear dma-buf.
            fn allocate(&mut self, content: &[u8]) -> Result<OwnedFd> {
                let size = content.len() as u64;
                let mut export_info = vk::ExportMemoryAllocateInfo::default()
                    .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
                let memory = unsafe {
                    self.device.allocate_memory(
                        &vk::MemoryAllocateInfo::default()
                            .allocation_size(size)
                            .memory_type_index(self.memory_type_index)
                            .push_next(&mut export_info),
                        None,
                    )
                }
                .context("allocate the producer buffer")?;

                unsafe {
                    let mapped = self
                        .device
                        .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
                        .context("map the producer buffer")? as *mut u8;
                    std::ptr::copy_nonoverlapping(content.as_ptr(), mapped, content.len());
                    self.device.unmap_memory(memory);
                }

                let fd = unsafe {
                    self.external_memory_fd.get_memory_fd(
                        &vk::MemoryGetFdInfoKHR::default()
                            .memory(memory)
                            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
                    )
                }
                .context("export the producer's dma-buf")?;
                let owned = unsafe { OwnedFd::from_raw_fd(fd) };
                self.memories.push(memory);
                Ok(owned)
            }
        }

        impl Drop for Vulkan {
            fn drop(&mut self) {
                unsafe {
                    self.device.destroy_device(None);
                    self.instance.destroy_instance(None);
                }
            }
        }
    }

    /// Windows' producer: Direct3D 11 on the device the window's renderer lends — the route a
    /// hardware decoder or a Direct3D engine takes.
    #[cfg(target_os = "windows")]
    mod directx_backend {
        use anyhow::{Context as _, Result};
        use gpui::{
            AnyElement, DirectXSource, SurfaceSource, Window, gpu_canvas, prelude::*, surface,
        };
        use windows::Win32::Graphics::Direct3D::D3D_SRV_DIMENSION_TEXTURE2D;
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_SHADER_RESOURCE_VIEW_DESC,
            D3D11_SHADER_RESOURCE_VIEW_DESC_0, D3D11_TEX2D_SRV, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_DEFAULT, ID3D11Device, ID3D11ShaderResourceView, ID3D11Texture2D,
        };
        use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

        use super::{OLIVE, TILE, error_panel, panel};

        pub struct Producer {
            /// `surface(DirectXSource::Texture)`: the renderer makes the view.
            texture_variant: ID3D11Texture2D,
            /// `surface(DirectXSource::View)`: the producer made the view.
            view_variant: ID3D11ShaderResourceView,
            /// `gpu_canvas(..)`: the view handed over at paint time.
            canvas_variant: ID3D11ShaderResourceView,
        }

        impl Producer {
            fn new(window: &mut Window) -> Result<Self> {
                // The renderer owns the device a surface texture has to be made on, and lends it.
                let device = window
                    .device_any()
                    .and_then(|any| any.downcast::<ID3D11Device>().ok())
                    .context("the window's renderer lends an ID3D11Device")?;

                let texture_variant = olive_texture(&device)?;
                let view_texture = olive_texture(&device)?;
                let view_variant = shader_resource_view(&device, &view_texture)?;
                let canvas_texture = olive_texture(&device)?;
                let canvas_variant = shader_resource_view(&device, &canvas_texture)?;

                Ok(Self {
                    texture_variant,
                    view_variant,
                    canvas_variant,
                })
            }
        }

        pub fn tiles(slot: &mut Option<Producer>, window: &mut Window) -> Vec<AnyElement> {
            if slot.is_none() {
                match Producer::new(window) {
                    Ok(producer) => *slot = Some(producer),
                    Err(error) => {
                        log::error!("surface: cannot produce a Direct3D texture: {error:#}");
                        return vec![error_panel(format!(
                            "no Direct3D producer: {error:#}"
                        ))];
                    }
                }
            }
            let producer = slot.as_ref().expect("the producer was just built");
            let canvas_view = producer.canvas_variant.clone();

            vec![
                panel(
                    surface(SurfaceSource::DirectX(DirectXSource::Texture(
                        producer.texture_variant.clone(),
                    )))
                    .size_full(),
                    "surface() · DirectXSource::Texture",
                ),
                panel(
                    surface(SurfaceSource::DirectX(DirectXSource::View(
                        producer.view_variant.clone(),
                    )))
                    .size_full(),
                    "surface() · DirectXSource::View",
                ),
                panel(
                    gpu_canvas(move |gpu| {
                        gpu.paint_surface(SurfaceSource::DirectX(DirectXSource::View(canvas_view)));
                    })
                    .size_full(),
                    "gpu_canvas(..) · Direct3D 11 view",
                ),
            ]
        }

        /// Olive as `ClearRenderTargetView` takes it: RGBA floats, matching the renderer's own
        /// `B8G8R8A8_UNORM` interpretation.
        fn olive_clear_color() -> [f32; 4] {
            [
                f32::from(OLIVE[0]) / 255.0,
                f32::from(OLIVE[1]) / 255.0,
                f32::from(OLIVE[2]) / 255.0,
                1.0,
            ]
        }

        /// An offscreen texture on `device`, cleared to olive.
        ///
        /// `RENDER_TARGET` so it can be cleared; `SHADER_RESOURCE` because both layers sample it. The
        /// format is the renderer's own target format, `B8G8R8A8_UNORM`: the renderer views the texture
        /// as non-sRGB and the fragment samples its bytes straight through.
        fn olive_texture(device: &ID3D11Device) -> Result<ID3D11Texture2D> {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: TILE,
                Height: TILE,
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
            clear(device, &texture, olive_clear_color())?;
            Ok(texture)
        }

        /// Clears `texture` to `color`.
        ///
        /// The immediate context is the one the renderer draws through, so the clear is ordered before
        /// the draw that samples this: nothing to submit and nothing to wait on.
        fn clear(
            device: &ID3D11Device,
            texture: &ID3D11Texture2D,
            color: [f32; 4],
        ) -> Result<()> {
            unsafe {
                let mut render_target = None;
                device.CreateRenderTargetView(texture, None, Some(&mut render_target))?;
                let render_target =
                    render_target.context("CreateRenderTargetView returned no view")?;
                let context = device
                    .GetImmediateContext()
                    .context("the device has no immediate context")?;
                context.ClearRenderTargetView(&render_target, &color);
            }
            Ok(())
        }

        /// A non-sRGB `B8G8R8A8_UNORM` view of `texture`, the payload of a view tile.
        fn shader_resource_view(
            device: &ID3D11Device,
            texture: &ID3D11Texture2D,
        ) -> Result<ID3D11ShaderResourceView> {
            unsafe {
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
                device.CreateShaderResourceView(texture, Some(&desc), Some(&mut view))?;
                view.context("CreateShaderResourceView returned no view")
            }
        }
    }

    /// macOS' producer: a CoreVideo buffer for two tiles, and a wgpu texture resolved to its
    /// `id<MTLTexture>` for the third.
    #[cfg(target_os = "macos")]
    mod metal_backend {
        use std::sync::Arc;

        use anyhow::{Context as _, Result, ensure};
        use core_foundation::base::{CFType, TCFType as _};
        use core_foundation::boolean::CFBoolean;
        use core_foundation::dictionary::CFDictionary;
        use core_foundation::string::CFString;
        use core_video::pixel_buffer::{
            CVPixelBuffer, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        };
        use gpui::{
            AnyElement, Corners, ImportedTextureHandle, SurfaceSource, Window, gpu_canvas,
            prelude::*, surface,
        };

        use super::{OLIVE, TILE, error_panel, nv12_from_rgb, olive_bgra, panel, solid};

        pub struct Producer {
            /// The CoreVideo buffer, for `surface()` and `gpu_canvas(..)`.
            buffer: CVPixelBuffer,
            /// Held so the device outlives the view taken from the texture.
            _device: Arc<wgpu::Device>,
            _texture: wgpu::Texture,
            /// The imported-texture tile's handle.
            imported: ImportedTextureHandle,
        }

        impl Producer {
            fn new() -> Result<Self> {
                // A decoder's frame: an IOSurface-backed, two-plane NV12 buffer at full range, holding
                // olive made from the renderer's own BT.601 conversion.
                let buffer = new_nv12_buffer(TILE, TILE)?;
                fill_olive(&buffer)?;

                // The imported-texture tile: a wgpu texture on the one Metal device macOS hands every
                // creator, so it is the device the renderer samples on.
                let (device, queue) = device()?;
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("surface_showcase"),
                    size: wgpu::Extent3d {
                        width: TILE,
                        height: TILE,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // sRGB, the format the imported-texture path requires: the sampler decodes to
                    // linear and the fragment re-encodes, so the olive bytes come back unchanged.
                    format: wgpu::TextureFormat::Bgra8UnormSrgb,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &solid(TILE * TILE, olive_bgra()),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(TILE * 4),
                        rows_per_image: Some(TILE),
                    },
                    wgpu::Extent3d {
                        width: TILE,
                        height: TILE,
                        depth_or_array_layers: 1,
                    },
                );
                let imported = metal_handle(&texture)?;

                Ok(Self {
                    buffer,
                    _device: device,
                    _texture: texture,
                    imported,
                })
            }
        }

        pub fn tiles(slot: &mut Option<Producer>, _window: &mut Window) -> Vec<AnyElement> {
            if slot.is_none() {
                match Producer::new() {
                    Ok(producer) => *slot = Some(producer),
                    Err(error) => {
                        log::error!("surface: cannot produce the macOS payloads: {error:#}");
                        return vec![error_panel(format!("producer unavailable: {error:#}"))];
                    }
                }
            }
            let producer = slot.as_ref().expect("the producer was just built");
            let surface_buffer = producer.buffer.clone();
            let canvas_buffer = producer.buffer.clone();
            let imported = producer.imported.clone();

            vec![
                panel(
                    surface(SurfaceSource::CoreVideo(surface_buffer)).size_full(),
                    "surface() · CoreVideo CVPixelBuffer",
                ),
                panel(
                    gpu_canvas(move |gpu| {
                        gpu.paint_surface(SurfaceSource::CoreVideo(canvas_buffer));
                    })
                    .size_full(),
                    "gpu_canvas(..) · CoreVideo",
                ),
                panel(
                    gpu_canvas(move |gpu| {
                        gpu.paint_texture(imported, Corners::default(), 1.0, false);
                    })
                    .size_full(),
                    "gpu_canvas(..) · id<MTLTexture>",
                ),
            ]
        }

        /// An `IOSurface`-backed, two-plane `NV12` full-range buffer — the layout a hardware decoder
        /// produces and the renderer expects.
        fn new_nv12_buffer(width: u32, height: u32) -> Result<CVPixelBuffer> {
            // A `CVPixelBuffer` is IOSurface-backed only when its attributes ask for it; the renderer's
            // texture cache needs the IOSurface behind it. Metal compatibility is declared alongside.
            let iosurface_properties: CFDictionary<CFString, CFType> =
                CFDictionary::from_CFType_pairs(&[]);
            let iosurface_key: CFString =
                unsafe { CFString::wrap_under_get_rule(kCVPixelBufferIOSurfacePropertiesKey) };
            let metal_key: CFString =
                unsafe { CFString::wrap_under_get_rule(kCVPixelBufferMetalCompatibilityKey) };
            let attributes = CFDictionary::from_CFType_pairs(&[
                (iosurface_key, iosurface_properties.as_CFType()),
                (metal_key, CFBoolean::true_value().as_CFType()),
            ]);

            CVPixelBuffer::new(
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                width as usize,
                height as usize,
                Some(&attributes),
            )
            .map_err(|code| anyhow::anyhow!("CVPixelBuffer::new returned CVReturn {code}"))
        }

        /// Fill both planes with the olive bytes, padding included, so no stale bytes are sampled.
        fn fill_olive(buffer: &CVPixelBuffer) -> Result<()> {
            let (y, cb, cr) = nv12_from_rgb(OLIVE);
            let width = buffer.get_width();
            let height = buffer.get_height();

            // 0 is the read-write lock: the CPU fills the planes, the GPU samples them.
            let lock = 0u64;
            let result = buffer.lock_base_address(lock);
            ensure!(
                result == 0,
                "CVPixelBufferLockBaseAddress returned CVReturn {result}"
            );

            // Safety: the buffer is locked, so both planes' base addresses are valid for the plane's
            // height × stride, and the sampler reads the same memory after the unlock.
            unsafe {
                let luma = buffer.get_base_address_of_plane(0) as *mut u8;
                let luma_stride = buffer.get_bytes_per_row_of_plane(0);
                let chroma = buffer.get_base_address_of_plane(1) as *mut u8;
                let chroma_stride = buffer.get_bytes_per_row_of_plane(1);
                let chroma_height = buffer.get_height_of_plane(1);

                for row in 0..height {
                    std::ptr::write_bytes(luma.add(row * luma_stride), y, width);
                }
                // Cb and Cr are interleaved, one byte each.
                for row in 0..chroma_height {
                    let row_ptr = chroma.add(row * chroma_stride);
                    for pair in 0..(chroma_stride / 2) {
                        *row_ptr.add(pair * 2) = cb;
                        *row_ptr.add(pair * 2 + 1) = cr;
                    }
                }
            }

            let result = buffer.unlock_base_address(lock);
            ensure!(
                result == 0,
                "CVPixelBufferUnlockBaseAddress returned CVReturn {result}"
            );
            Ok(())
        }

        /// wgpu's own Metal device, which is the one the window's Metal renderer also draws through.
        fn device() -> Result<(Arc<wgpu::Device>, Arc<wgpu::Queue>)> {
            let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                backends: wgpu::Backends::METAL,
                flags: wgpu::InstanceFlags::default(),
                backend_options: wgpu::BackendOptions::default(),
                memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
                display: None,
            });
            let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            }))
            .map_err(|error| anyhow::anyhow!("no Metal adapter: {error}"))?;
            let (device, queue) =
                pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("surface_showcase"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                }))
                .map_err(|error| anyhow::anyhow!("no wgpu device: {error}"))?;
            Ok((Arc::new(device), Arc::new(queue)))
        }

        /// The texture's own `id<MTLTexture>`, the handle the renderer resolves.
        fn metal_handle(texture: &wgpu::Texture) -> Result<ImportedTextureHandle> {
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
}
