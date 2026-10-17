#[cfg(not(target_family = "wasm"))]
use gpui_platform::{GpuAdapterInfo, GpuBackend, GpuDeviceType, GpuPreference};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use wgpu::TextureFormat;

#[derive(Clone)]
pub struct WgpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    backend: WgpuBackend,
    dual_source_blending: bool,
    color_texture_format: wgpu::TextureFormat,
    errors: Arc<DeviceErrorState>,
}

/// Errors reported by wgpu's device-wide callbacks.
///
/// A device has exactly one lost callback and one uncaptured-error callback, so this
/// state is installed once per device and shared by every renderer using it. Renderers
/// keep their own handle so they can keep observing a loss after the context that
/// produced it has been dropped for recovery.
#[derive(Default)]
pub struct DeviceErrorState {
    lost: AtomicBool,
    last_error: Mutex<(u64, Option<String>)>,
}

impl DeviceErrorState {
    fn install(device: &wgpu::Device) -> Arc<Self> {
        let errors = Arc::new(Self::default());
        device.set_device_lost_callback({
            let errors = Arc::clone(&errors);
            move |reason, message| {
                log::error!("wgpu device lost: reason={reason:?}, message={message}");
                if reason != wgpu::DeviceLostReason::Destroyed {
                    errors.lost.store(true, Ordering::Relaxed);
                }
            }
        });
        device.on_uncaptured_error(Arc::new({
            let errors = Arc::clone(&errors);
            move |error| errors.record(error.to_string())
        }));
        errors
    }

    /// Returns true if the GPU device was lost (e.g., due to driver crash, suspend/resume).
    pub fn device_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }

    fn record(&self, error: String) {
        let mut last_error = self.last_error.lock();
        last_error.0 = last_error.0.wrapping_add(1);
        last_error.1 = Some(error);
    }

    /// Returns the latest error once per observer, without consuming shared state.
    /// Start each observer at zero, including after switching devices, so errors
    /// raised during construction are observed. Multiple errors between observations
    /// coalesce into the latest error; this is not an error queue.
    pub fn observe_error(&self, generation: &mut u64) -> Option<String> {
        let last_error = self.last_error.lock();
        if *generation == last_error.0 {
            return None;
        }
        *generation = last_error.0;
        last_error.1.clone()
    }

    /// The generation an observer should start from to see only errors raised after
    /// this call, for a renderer joining a device other renderers already used.
    pub fn current_generation(&self) -> u64 {
        self.last_error.lock().0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WgpuBackend {
    BrowserWebGpu,
    Gl,
    Native(wgpu::Backend),
}

#[cfg(target_family = "wasm")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WebBackendPreference {
    #[default]
    Auto,
    WebGpu,
    WebGl,
}

#[cfg(target_family = "wasm")]
pub struct PreparedWebGraphics {
    pub context: WgpuContext,
    pub surface: wgpu::Surface<'static>,
}

/// wgpu-core refuses to create a surface when neither the instance nor the surface
/// target carries a display handle, and `SurfaceTarget::Canvas` always passes `None`.
/// The WebGL2 backend never reads the handle (WebGPU bypasses wgpu-core entirely), so
/// a unit web display handle on the instance satisfies the check.
#[cfg(target_family = "wasm")]
#[derive(Debug)]
struct WebDisplaySource;

#[cfg(target_family = "wasm")]
impl raw_window_handle::HasDisplayHandle for WebDisplaySource {
    fn display_handle(
        &self,
    ) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::web())
    }
}

#[derive(Clone, Copy)]
pub struct CompositorGpuHint {
    pub vendor_id: u32,
    pub device_id: u32,
}

/// Describe one adapter in the terms `gpui_platform` publishes.
///
/// `wgpu`'s vocabulary stops at this crate's boundary: `gpui_platform` is compiled for backends
/// that have no `wgpu`, and an application choosing a GPU should not have to depend on the one the
/// Linux renderer happens to use.
#[cfg(not(target_family = "wasm"))]
pub fn adapter_info(info: &wgpu::AdapterInfo) -> GpuAdapterInfo {
    GpuAdapterInfo {
        name: info.name.clone(),
        device_type: match info.device_type {
            wgpu::DeviceType::DiscreteGpu => GpuDeviceType::Discrete,
            wgpu::DeviceType::IntegratedGpu => GpuDeviceType::Integrated,
            wgpu::DeviceType::VirtualGpu => GpuDeviceType::Virtual,
            wgpu::DeviceType::Cpu => GpuDeviceType::Cpu,
            wgpu::DeviceType::Other => GpuDeviceType::Other,
        },
        vendor_id: info.vendor,
        device_id: info.device,
        backend: match info.backend {
            wgpu::Backend::Vulkan => GpuBackend::Vulkan,
            wgpu::Backend::Metal => GpuBackend::Metal,
            wgpu::Backend::Dx12 => GpuBackend::Dx12,
            wgpu::Backend::Gl => GpuBackend::Gl,
            wgpu::Backend::BrowserWebGpu => GpuBackend::BrowserWebGpu,
            wgpu::Backend::Noop => GpuBackend::Other,
        },
        is_software: info.device_type == wgpu::DeviceType::Cpu,
    }
}

/// Every adapter this process could render on.
///
/// It creates an instance and enumerates, but no device: that is the cost enumeration exists to
/// avoid, and the reason the answer carries identity and class only. A caller that already has a
/// context should use [`enumerate_adapters_from`] with its instance, so the list is the one that
/// context could actually have chosen from.
#[cfg(not(target_family = "wasm"))]
pub fn enumerate_adapters() -> Vec<GpuAdapterInfo> {
    enumerate_adapters_from(&WgpuContext::instance(None))
}

/// Every adapter `instance` reports, in the backend's order.
#[cfg(not(target_family = "wasm"))]
pub fn enumerate_adapters_from(instance: &wgpu::Instance) -> Vec<GpuAdapterInfo> {
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
        .iter()
        .map(|adapter| adapter_info(&adapter.get_info()))
        .collect()
}

impl WgpuContext {
    /// Describe the adapter this context renders on, in the terms `gpui_platform` publishes.
    #[cfg(not(target_family = "wasm"))]
    pub fn adapter_info(&self) -> GpuAdapterInfo {
        adapter_info(&self.adapter.get_info())
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn new(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        compositor_gpu: Option<CompositorGpuHint>,
        gpu_preference: GpuPreference,
    ) -> anyhow::Result<Self> {
        Self::new_with_options(instance, surface, compositor_gpu, gpu_preference, false)
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn new_rejecting_software(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        compositor_gpu: Option<CompositorGpuHint>,
        gpu_preference: GpuPreference,
    ) -> anyhow::Result<Self> {
        Self::new_with_options(instance, surface, compositor_gpu, gpu_preference, true)
    }

    #[cfg(not(target_family = "wasm"))]
    fn new_with_options(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        compositor_gpu: Option<CompositorGpuHint>,
        gpu_preference: GpuPreference,
        reject_software: bool,
    ) -> anyhow::Result<Self> {
        // Select an adapter by actually testing surface configuration with the real device.
        // This is the only reliable way to determine compatibility on hybrid GPU systems.
        let (adapter, device, queue, dual_source_blending, color_texture_format) =
            pollster::block_on(Self::select_adapter_and_device(
                &instance,
                &gpu_preference,
                surface,
                compositor_gpu.as_ref(),
                reject_software,
            ))?;

        Ok(Self::from_native_adapter_and_device(
            instance,
            adapter,
            device,
            queue,
            dual_source_blending,
            color_texture_format,
        ))
    }

    #[cfg(all(
        not(target_family = "wasm"),
        any(test, feature = "bench-support", feature = "test-support", feature = "headless")
    ))]
    pub(crate) fn new_headless() -> anyhow::Result<(Self, wgpu::TextureFormat)> {
        // Creating a device takes tens of milliseconds, and benchmarks create a headless
        // renderer for every window they open, so the process shares one device until it
        // is lost. A static rather than a thread-local: the cached device is never dropped,
        // because wgpu's queue reads its own thread-locals on drop and would panic if
        // dropped while a thread's thread-locals are being destroyed.
        static HEADLESS: Mutex<Option<(WgpuContext, wgpu::TextureFormat)>> = Mutex::new(None);
        let mut headless = HEADLESS.lock();
        if let Some((context, format)) = headless.as_ref()
            && !context.device_lost()
        {
            return Ok((context.clone(), *format));
        }
        let created = Self::create_headless()?;
        *headless = Some(created.clone());
        Ok(created)
    }

    #[cfg(all(
        not(target_family = "wasm"),
        any(test, feature = "bench-support", feature = "test-support", feature = "headless")
    ))]
    fn create_headless() -> anyhow::Result<(Self, wgpu::TextureFormat)> {
        let instance = Self::instance(None);
        // Headless has no platform to state a preference, so `ZED_DEVICE_ID` is the only override.
        // Without it, keep `Auto`'s ranking — what the tests and benchmarks expect.
        let gpu_preference = GpuPreference::from_env().unwrap_or(GpuPreference::Auto);
        let (adapter, device, queue, dual_source_blending, color_texture_format, target_format) =
            pollster::block_on(async {
                let mut adapters = instance.enumerate_adapters(wgpu::Backends::all()).await;
                Self::order_adapters(&mut adapters, &gpu_preference, None)?;

                for adapter in adapters {
                    let adapter_info = adapter.get_info();
                    let Some(target_format) = Self::headless_target_format(&adapter) else {
                        log::warn!(
                            "Adapter {:?} has no supported headless render target format",
                            adapter_info.name
                        );
                        continue;
                    };

                    match Self::create_device(&adapter).await {
                        Ok((device, queue, dual_source_blending, color_texture_format)) => {
                            #[cfg(feature = "bench-support")]
                            if adapter_info.device_type == wgpu::DeviceType::Cpu {
                                log::error!(
                                    "Headless renderer selected software adapter {:?}; \
                                     benchmark results measure CPU software rendering, not hardware GPU rendering",
                                    adapter_info.name
                                );
                            }
                            return Ok((
                                adapter,
                                device,
                                queue,
                                dual_source_blending,
                                color_texture_format,
                                target_format,
                            ));
                        }
                        Err(error) => {
                            log::warn!(
                                "Failed to create a headless device for adapter {:?}: {error:#}",
                                adapter_info.name
                            );
                        }
                    }
                }

                anyhow::bail!("No usable headless GPU adapter found")
            })?;

        Ok((
            Self::from_native_adapter_and_device(
                instance,
                adapter,
                device,
                queue,
                dual_source_blending,
                color_texture_format,
            ),
            target_format,
        ))
    }

    #[cfg(all(
        not(target_family = "wasm"),
        any(test, feature = "bench-support", feature = "test-support", feature = "headless")
    ))]
    /// Both candidates are 8-bit RGBA-ordered or BGRA-ordered formats: headless readback
    /// copies rows as 4 bytes per pixel and only swizzles, so no other formats may be added
    /// here without updating it.
    fn headless_target_format(adapter: &wgpu::Adapter) -> Option<wgpu::TextureFormat> {
        let required_usages =
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ]
        .into_iter()
        .find(|format| {
            adapter
                .get_texture_format_features(*format)
                .allowed_usages
                .contains(required_usages)
        })
    }

    #[cfg(not(target_family = "wasm"))]
    fn from_native_adapter_and_device(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        dual_source_blending: bool,
        color_texture_format: TextureFormat,
    ) -> Self {
        let errors = DeviceErrorState::install(&device);

        log::info!(
            "Selected GPU adapter: {:?} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );
        let backend = WgpuBackend::Native(adapter.get_info().backend);

        Self {
            instance,
            adapter,
            device: Arc::new(device),
            queue: Arc::new(queue),
            backend,
            dual_source_blending,
            color_texture_format,
            errors,
        }
    }

    #[cfg(target_family = "wasm")]
    pub async fn new_web(
        canvas: &web_sys::HtmlCanvasElement,
        preference: WebBackendPreference,
    ) -> anyhow::Result<PreparedWebGraphics> {
        Self::new_web_with_backend(canvas, preference).await
    }

    #[cfg(target_family = "wasm")]
    #[allow(clippy::arc_with_non_send_sync)]
    async fn new_web_with_backend(
        canvas: &web_sys::HtmlCanvasElement,
        preference: WebBackendPreference,
    ) -> anyhow::Result<PreparedWebGraphics> {
        let backends = match preference {
            WebBackendPreference::Auto => wgpu::Backends::BROWSER_WEBGPU | wgpu::Backends::GL,
            WebBackendPreference::WebGpu => wgpu::Backends::BROWSER_WEBGPU,
            WebBackendPreference::WebGl => wgpu::Backends::GL,
        };
        let descriptor = wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: Some(Box::new(WebDisplaySource)),
        };
        let instance = if preference == WebBackendPreference::Auto {
            wgpu::util::new_instance_with_webgpu_detection(descriptor).await
        } else {
            wgpu::Instance::new(descriptor)
        };
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|error| {
                anyhow::anyhow!("Failed to create browser graphics surface: {error}")
            })?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "Failed to request a {preference:?} adapter compatible with the canvas: {error}"
                )
            })?;
        let adapter_info = adapter.get_info();
        let backend = match adapter_info.backend {
            wgpu::Backend::BrowserWebGpu => WgpuBackend::BrowserWebGpu,
            wgpu::Backend::Gl => WgpuBackend::Gl,
            backend => {
                anyhow::bail!(
                    "Browser graphics initialization selected unexpected backend {backend:?}"
                )
            }
        };

        let (device, queue, dual_source_blending, color_texture_format) =
            Self::create_device(&adapter).await?;
        let errors = DeviceErrorState::install(&device);
        log::info!(
            "Browser graphics initialized: requested={preference:?}, selected={backend:?}, \
             adapter={:?}, limits={:?}, dual_source_blending={dual_source_blending}",
            adapter_info.name,
            device.limits(),
        );

        let context = Self {
            instance,
            adapter,
            device: Arc::new(device),
            queue: Arc::new(queue),
            backend,
            dual_source_blending,
            color_texture_format,
            errors,
        };
        Ok(PreparedWebGraphics { context, surface })
    }

    async fn create_device(
        adapter: &wgpu::Adapter,
    ) -> anyhow::Result<(wgpu::Device, wgpu::Queue, bool, TextureFormat)> {
        let dual_source_blending = adapter
            .features()
            .contains(wgpu::Features::DUAL_SOURCE_BLENDING);

        let mut required_features = wgpu::Features::empty();
        if dual_source_blending {
            required_features |= wgpu::Features::DUAL_SOURCE_BLENDING;
        } else {
            log::warn!(
                "Dual-source blending not available on this GPU. \
                Subpixel text antialiasing will be disabled."
            );
        }

        let color_atlas_texture_format = Self::select_color_texture_format(adapter)?;

        // A `Nv12` surface is one multi-planar image, a native format the device must be allowed to
        // create; without the feature the import is refused and the surface dropped, rather than
        // falling back to a pair of single-plane images the modifier does not describe.
        if adapter
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_NV12)
        {
            required_features |= wgpu::Features::TEXTURE_FORMAT_NV12;
        }
        #[cfg(target_family = "wasm")]
        let required_limits = if adapter.get_info().backend == wgpu::Backend::Gl {
            wgpu::Limits::downlevel_webgl2_defaults()
                .using_resolution(adapter.limits())
                .using_alignment(adapter.limits())
        } else {
            wgpu::Limits::downlevel_defaults()
                .using_resolution(adapter.limits())
                .using_alignment(adapter.limits())
        };
        #[cfg(not(target_family = "wasm"))]
        let required_limits = wgpu::Limits::downlevel_defaults()
            .using_resolution(adapter.limits())
            .using_alignment(adapter.limits());

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("gpui_device"),
            required_features,
            required_limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        };

        // A producer's surface can carry a DRM format modifier, and wgpu's own device never enables
        // `VK_EXT_image_drm_format_modifier`, so the import is refused. Where the platform allows it
        // the device is created through the HAL instead, with that one extension added, and wgpu
        // adopts it unchanged; see `device_with_surface_import`. The returned device is wgpu's own
        // either way, so nothing downstream changes.
        #[cfg(target_os = "linux")]
        let with_surface_import = Self::device_with_surface_import(adapter, &descriptor);
        #[cfg(not(target_os = "linux"))]
        let with_surface_import: Option<(wgpu::Device, wgpu::Queue)> = None;

        let (device, queue) = match with_surface_import {
            Some(created) => created,
            None => adapter
                .request_device(&descriptor)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create wgpu device: {e}"))?,
        };

        Ok((
            device,
            queue,
            dual_source_blending,
            color_atlas_texture_format,
        ))
    }

    /// Create the window's device through the HAL, enabling `VK_EXT_image_drm_format_modifier`.
    ///
    /// wgpu's `Adapter::request_device` never enables it, and its absence is what makes the importer
    /// in `dmabuf.rs` refuse a tiled surface: a `DRM_FORMAT_MODIFIER_EXT` image cannot be created on a
    /// device that does not have the extension. wgpu-hal exposes the two halves needed to create a
    /// device that does — `Adapter::open_with_callback` mutates the extension list before
    /// `vkCreateDevice`, and `wgpu::Adapter::create_device_from_hal` adopts the result back into a
    /// `wgpu::Device` — so the extension is genuinely enabled, not assumed.
    ///
    /// `None` when the adapter is not Vulkan, when the driver does not support the extension, or when
    /// either call refuses; the caller then creates the device the ordinary way and the import keeps
    /// to linear buffers, as it did before.
    #[cfg(target_os = "linux")]
    fn device_with_surface_import(
        adapter: &wgpu::Adapter,
        descriptor: &wgpu::DeviceDescriptor<'_>,
    ) -> Option<(wgpu::Device, wgpu::Queue)> {
        use wgpu::hal::vulkan::Api;

        // SAFETY: the HAL device is created from this adapter with the adapter's own features and
        // limits (exactly what `descriptor` carries), and the callback adds only an extension the
        // adapter reports it supports and never removes one. `create_device_from_hal` requires the
        // HAL device to come from this adapter, which it does.
        unsafe {
            let hal = adapter.as_hal::<Api>()?;
            if !hal
                .physical_device_capabilities()
                .supports_extension(ash::vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME)
            {
                return None;
            }
            let open = hal
                .open_with_callback(
                    descriptor.required_features,
                    &descriptor.required_limits,
                    &descriptor.memory_hints,
                    Some(Box::new(|args| {
                        args.extensions
                            .push(ash::vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME);
                    })),
                )
                .ok()?;
            let (device, queue) = adapter
                .create_device_from_hal::<Api>(open, descriptor)
                .ok()?;
            // The extension is the whole point of this path; if the device did not take it, fall back
            // so the caller builds the ordinary device rather than leave the importer assuming a
            // capability the device does not have.
            let enabled = device.as_hal::<Api>().is_some_and(|device| {
                device
                    .enabled_device_extensions()
                    .contains(&ash::vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME)
            });
            enabled.then_some((device, queue))
        }
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn instance(display: Option<Box<dyn wgpu::wgt::WgpuHasDisplayHandle>>) -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: Self::backends(),
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display,
        })
    }

    /// The backends the instance is built with.
    ///
    /// Vulkan and GL are what this asked for while Linux was the only host. Neither can present
    /// on the other desktop platforms: on Windows they find no adapter at all, and on macOS they
    /// cannot even build a surface from the view's raw handle. So each adds the API its window
    /// system actually has. Without them `WgpuRenderer::new` fails at instance creation, before
    /// a frame is reached, which is not something a window can recover from.
    #[cfg(not(target_family = "wasm"))]
    fn backends() -> wgpu::Backends {
        let mut backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        if cfg!(target_os = "windows") {
            backends |= wgpu::Backends::DX12;
        }
        if cfg!(target_os = "macos") {
            backends |= wgpu::Backends::METAL;
        }
        backends
    }

    pub fn check_compatible_with_surface(&self, surface: &wgpu::Surface<'_>) -> anyhow::Result<()> {
        let caps = surface.get_capabilities(&self.adapter);
        if caps.formats.is_empty() {
            let info = self.adapter.get_info();
            anyhow::bail!(
                "Adapter {:?} (backend={:?}, device={:#06x}) is not compatible with the \
                 display surface for this window.",
                info.name,
                info.backend,
                info.device,
            );
        }
        Ok(())
    }

    /// Select an adapter and create a device, testing that the surface can actually be configured.
    /// This is the only reliable way to determine compatibility on hybrid GPU systems, where
    /// adapters may report surface compatibility via get_capabilities() but fail when actually
    /// configuring (e.g., NVIDIA reporting Vulkan Wayland support but failing because the
    /// Wayland compositor runs on the Intel GPU).
    #[cfg(not(target_family = "wasm"))]
    async fn select_adapter_and_device(
        instance: &wgpu::Instance,
        gpu_preference: &GpuPreference,
        surface: &wgpu::Surface<'_>,
        compositor_gpu: Option<&CompositorGpuHint>,
        reject_software: bool,
    ) -> anyhow::Result<(
        wgpu::Adapter,
        wgpu::Device,
        wgpu::Queue,
        bool,
        TextureFormat,
    )> {
        let mut adapters: Vec<_> = instance.enumerate_adapters(wgpu::Backends::all()).await;

        if adapters.is_empty() {
            anyhow::bail!("No GPU adapters found");
        }

        // `Custom` names one adapter rather than ordering them, so it is resolved before the
        // ranking: the chosen adapter goes first and the rest stay as fallbacks in `Auto`'s order.
        // Resolving it here means the surface test below still gets a chance to reject the choice and
        // try the next, while the application can read back what it actually got.
        Self::order_adapters(&mut adapters, gpu_preference, compositor_gpu)?;

        if let GpuPreference::Device(gpu_id) = gpu_preference {
            log::info!("GPU preference: device {:#06x}", gpu_id.device_id);
        }

        Self::sort_adapters(&mut adapters, gpu_preference, compositor_gpu);

        // Log all available adapters (in sorted order)
        log::info!("Found {} GPU adapter(s):", adapters.len());
        for adapter in &adapters {
            let info = adapter.get_info();
            log::info!(
                "  - {} (vendor={:#06x}, device={:#06x}, backend={:?}, type={:?})",
                info.name,
                info.vendor,
                info.device,
                info.backend,
                info.device_type,
            );
        }

        // Test each adapter by creating a device and configuring the surface
        for adapter in adapters {
            let info = adapter.get_info();

            if reject_software && info.device_type == wgpu::DeviceType::Cpu {
                log::info!(
                    "Skipping software renderer: {} ({:?})",
                    info.name,
                    info.backend
                );
                continue;
            }

            log::info!("Testing adapter: {} ({:?})...", info.name, info.backend);

            match Self::try_adapter_with_surface(&adapter, surface).await {
                Ok((device, queue, dual_source_blending, color_atlas_texture_format)) => {
                    log::info!(
                        "Selected GPU (passed configuration test): {} ({:?})",
                        info.name,
                        info.backend
                    );
                    return Ok((
                        adapter,
                        device,
                        queue,
                        dual_source_blending,
                        color_atlas_texture_format,
                    ));
                }
                Err(e) => {
                    log::info!(
                        "  Adapter {} ({:?}) failed: {}, trying next...",
                        info.name,
                        info.backend,
                        e
                    );
                }
            }
        }

        anyhow::bail!("No GPU adapter found that can configure the display surface")
    }

    /// Order `adapters` for `preference`, resolving a custom choice first.
    ///
    /// A custom preference names one adapter, so it is not part of the ranking: the chosen adapter is
    /// moved to the front and the rest keep `Auto`'s order as fallbacks. Everything else goes through
    /// [`sort_adapters`].
    #[cfg(not(target_family = "wasm"))]
    fn order_adapters(
        adapters: &mut Vec<wgpu::Adapter>,
        preference: &GpuPreference,
        compositor_gpu: Option<&CompositorGpuHint>,
    ) -> anyhow::Result<()> {
        let GpuPreference::Custom(selector) = preference else {
            Self::sort_adapters(adapters, preference, compositor_gpu);
            return Ok(());
        };

        let info: Vec<GpuAdapterInfo> = adapters
            .iter()
            .map(|adapter| adapter_info(&adapter.get_info()))
            .collect();
        let chosen = custom_choice(&info, selector.as_ref())?;
        let adapter = adapters.remove(chosen);
        // The tail is a fallback, not a ranked preference, so it keeps the historical order: if the
        // display cannot be configured on the chosen adapter, the next best one is tried.
        Self::sort_adapters(adapters, &GpuPreference::Auto, compositor_gpu);
        adapters.insert(0, adapter);
        Ok(())
    }

    /// Sort adapters into the order `preference` asks for.
    ///
    /// The slot meanings are in [`adapter_key`]. Under `Auto` the tiers are the historical ones: the
    /// compositor's hint, then device type (discrete > integrated > other > virtual > CPU), then
    /// backend. `LowPower` and `HighPerformance` replace the type order and keep the hint as a
    /// tie-break; `Device` ranks the named adapter first and otherwise falls back to `Auto`.
    #[cfg(not(target_family = "wasm"))]
    fn sort_adapters(
        adapters: &mut [wgpu::Adapter],
        gpu_preference: &GpuPreference,
        compositor_gpu: Option<&CompositorGpuHint>,
    ) {
        adapters.sort_by_cached_key(|adapter| {
            adapter_key(
                &adapter_info(&adapter.get_info()),
                gpu_preference,
                compositor_gpu,
            )
        });
    }

    /// Try to use an adapter with a surface by creating a device and testing configuration.
    /// Returns the device and queue if successful, allowing them to be reused.
    #[cfg(not(target_family = "wasm"))]
    async fn try_adapter_with_surface(
        adapter: &wgpu::Adapter,
        surface: &wgpu::Surface<'_>,
    ) -> anyhow::Result<(wgpu::Device, wgpu::Queue, bool, TextureFormat)> {
        let caps = surface.get_capabilities(adapter);
        if caps.formats.is_empty() {
            anyhow::bail!("no compatible surface formats");
        }
        if caps.alpha_modes.is_empty() {
            anyhow::bail!("no compatible alpha modes");
        }

        let (device, queue, dual_source_blending, color_atlas_texture_format) =
            Self::create_device(adapter).await?;
        let error_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let test_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: caps.formats[0],
            width: 64,
            height: 64,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };

        surface.configure(&device, &test_config);

        let error = error_scope.pop().await;
        if let Some(e) = error {
            anyhow::bail!("surface configuration failed: {e}");
        }

        Ok((
            device,
            queue,
            dual_source_blending,
            color_atlas_texture_format,
        ))
    }

    fn select_color_texture_format(adapter: &wgpu::Adapter) -> anyhow::Result<wgpu::TextureFormat> {
        let required_usages = wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST;
        let bgra_features = adapter.get_texture_format_features(wgpu::TextureFormat::Bgra8Unorm);
        let rgba_features = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm);
        #[cfg(target_family = "wasm")]
        if adapter.get_info().backend == wgpu::Backend::Gl
            && rgba_features.allowed_usages.contains(required_usages)
        {
            return Ok(wgpu::TextureFormat::Rgba8Unorm);
        }
        if bgra_features.allowed_usages.contains(required_usages) {
            return Ok(wgpu::TextureFormat::Bgra8Unorm);
        }
        if rgba_features.allowed_usages.contains(required_usages) {
            let info = adapter.get_info();
            log::warn!(
                "Adapter {} ({:?}) does not support Bgra8Unorm atlas textures with usages {:?}; \
                 falling back to Rgba8Unorm atlas textures.",
                info.name,
                info.backend,
                required_usages,
            );
            return Ok(wgpu::TextureFormat::Rgba8Unorm);
        }

        let info = adapter.get_info();
        Err(anyhow::anyhow!(
            "Adapter {} ({:?}, device={:#06x}) does not support a usable color atlas texture \
             format with usages {:?}. Bgra8Unorm allowed usages: {:?}; \
             Rgba8Unorm allowed usages: {:?}.",
            info.name,
            info.backend,
            info.device,
            required_usages,
            bgra_features.allowed_usages,
            rgba_features.allowed_usages,
        ))
    }
    pub fn backend(&self) -> WgpuBackend {
        self.backend
    }

    pub fn uses_webgl_instance_data(&self) -> bool {
        matches!(self.backend, WgpuBackend::Gl) && cfg!(target_family = "wasm")
    }

    pub fn supports_dual_source_blending(&self) -> bool {
        self.dual_source_blending
    }

    pub fn color_texture_format(&self) -> wgpu::TextureFormat {
        self.color_texture_format
    }

    /// Returns true if the GPU device was lost (e.g., due to driver crash, suspend/resume).
    /// When this returns true, the context should be recreated.
    pub fn device_lost(&self) -> bool {
        self.errors.device_lost()
    }

    /// The device-wide error state, shared with renderers on this device.
    pub fn errors(&self) -> &Arc<DeviceErrorState> {
        &self.errors
    }
}

/// Resolve a custom preference against the adapters that were enumerated.
///
/// The selector chooses one adapter out of the whole list — it does not rank them. `None`, or an
/// index outside the list, is an error rather than a fallback: the application and this code saw the
/// same enumeration, so a disagreement is a bug worth naming instead of silently drawing on a GPU
/// the application did not choose.
#[cfg(not(target_family = "wasm"))]
fn custom_choice(
    adapters: &[GpuAdapterInfo],
    selector: &dyn Fn(&[GpuAdapterInfo]) -> Option<usize>,
) -> anyhow::Result<usize> {
    match selector(adapters) {
        Some(index) if index < adapters.len() => Ok(index),
        Some(index) => anyhow::bail!(
            "The application's custom GPU preference chose adapter {index}, but only {} adapter(s) \
             were enumerated",
            adapters.len()
        ),
        None => anyhow::bail!("The application's custom GPU preference chose no adapter"),
    }
}

/// The sort key for one adapter under `preference`.
///
/// Slots are compared in order, and what they hold depends on the preference:
///
/// | preference | slot 0 | slot 1 | slot 2 | slot 3 |
/// | --- | --- | --- | --- | --- |
/// | `Auto` | 0 | compositor match | device type (discrete first) | backend |
/// | `Device` | named match | compositor match | device type (discrete first) | backend |
/// | `LowPower` / `HighPerformance` | 0 | device type (by preference) | compositor match | backend |
///
/// `Custom` never reaches this function: it names one adapter, which
/// [`order_adapters`](WgpuContext::order_adapters) resolves before sorting. The arm below keeps the
/// match exhaustive and ranks such an adapter as `Auto` would.
#[cfg(not(target_family = "wasm"))]
fn adapter_key(
    info: &GpuAdapterInfo,
    preference: &GpuPreference,
    compositor_gpu: Option<&CompositorGpuHint>,
) -> (u8, u8, u8, u8) {
    // Backends like OpenGL report device=0 for all adapters, so device-based matching is only
    // meaningful when non-zero.
    let device_known = info.device_id != 0;
    let compositor_match: u8 = match compositor_gpu {
        Some(hint)
            if device_known
                && info.vendor_id == hint.vendor_id
                && info.device_id == hint.device_id =>
        {
            0
        }
        _ => 1,
    };
    let auto_type: u8 = match info.device_type {
        GpuDeviceType::Discrete => 0,
        GpuDeviceType::Integrated => 1,
        GpuDeviceType::Other => 2,
        GpuDeviceType::Virtual => 3,
        GpuDeviceType::Cpu => 4,
    };
    let backend_priority: u8 = match info.backend {
        GpuBackend::Vulkan | GpuBackend::Metal | GpuBackend::Dx12 => 0,
        _ => 1,
    };
    match preference {
        GpuPreference::Auto | GpuPreference::Custom(_) => {
            (0, compositor_match, auto_type, backend_priority)
        }
        GpuPreference::Device(gpu_id) => {
            let named: u8 = if device_known && gpu_id.matches(info.vendor_id, info.device_id) {
                0
            } else {
                1
            };
            (named, compositor_match, auto_type, backend_priority)
        }
        GpuPreference::LowPower => (
            0,
            type_priority(info.device_type, true),
            compositor_match,
            backend_priority,
        ),
        GpuPreference::HighPerformance => (
            0,
            type_priority(info.device_type, false),
            compositor_match,
            backend_priority,
        ),
    }
}

/// The device-type order under an explicit power preference. A CPU adapter ranks last in both: it is
/// a fallback, not a low-power device.
#[cfg(not(target_family = "wasm"))]
fn type_priority(device_type: GpuDeviceType, low_power: bool) -> u8 {
    match (device_type, low_power) {
        (GpuDeviceType::Integrated, true) => 0,
        (GpuDeviceType::Discrete, false) => 0,
        (GpuDeviceType::Other, _) => 1,
        (GpuDeviceType::Integrated, false) => 2,
        (GpuDeviceType::Discrete, true) => 2,
        (GpuDeviceType::Virtual, _) => 3,
        (GpuDeviceType::Cpu, _) => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceErrorState;

    /// The escape hatch that the whole tiled-surface path rests on: a device created through the HAL
    /// with `VK_EXT_image_drm_format_modifier` added must come back with the extension enabled.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_device_enables_the_drm_format_modifier_extension() {
        use wgpu::hal::vulkan::Api;

        let instance = super::WgpuContext::instance(None);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
        }));
        let Ok(adapter) = adapter else {
            eprintln!("skipping: no adapter");
            return;
        };
        let supported = unsafe {
            adapter.as_hal::<Api>().is_some_and(|hal| {
                hal.physical_device_capabilities()
                    .supports_extension(ash::vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME)
            })
        };
        if !supported {
            eprintln!(
                "skipping: {:?} does not support the extension",
                adapter.get_info().name
            );
            return;
        }

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("drm modifier extension test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults()
                .using_resolution(adapter.limits())
                .using_alignment(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        };
        let (device, _queue) =
            super::WgpuContext::device_with_surface_import(&adapter, &descriptor)
                .expect("the escape hatch creates the device");
        let enabled = unsafe { device.as_hal::<Api>() }.is_some_and(|device| {
            device
                .enabled_device_extensions()
                .contains(&ash::vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME)
        });
        assert!(enabled, "the created device must have the extension enabled");
    }

    #[test]
    fn device_errors_are_observed_independently() {
        let errors = DeviceErrorState::default();
        let mut first = 0;
        let mut second = 0;
        assert_eq!(errors.observe_error(&mut first), None);
        errors.record("first".into());
        assert_eq!(errors.observe_error(&mut first).as_deref(), Some("first"));
        assert_eq!(errors.observe_error(&mut first), None);
        assert_eq!(errors.observe_error(&mut second).as_deref(), Some("first"));
        assert_eq!(errors.observe_error(&mut second), None);

        errors.record("second".into());
        assert_eq!(errors.observe_error(&mut second).as_deref(), Some("second"));
        assert_eq!(errors.observe_error(&mut first).as_deref(), Some("second"));
        assert_eq!(errors.observe_error(&mut first), None);
        assert_eq!(errors.observe_error(&mut second), None);
    }

    #[test]
    fn device_errors_coalesce_and_new_observers_see_latest() {
        let errors = DeviceErrorState::default();
        errors.record("older".into());
        errors.record("latest".into());
        let mut generation = 0;
        assert_eq!(
            errors.observe_error(&mut generation).as_deref(),
            Some("latest")
        );
        assert_eq!(errors.observe_error(&mut generation), None);

        let replacement = DeviceErrorState::default();
        replacement.record("replacement".into());
        generation = 0;
        assert_eq!(
            replacement.observe_error(&mut generation).as_deref(),
            Some("replacement")
        );
        assert_eq!(replacement.observe_error(&mut generation), None);
    }

    #[cfg(not(target_family = "wasm"))]
    fn facts(
        name: &str,
        device_type: gpui_platform::GpuDeviceType,
        vendor: u32,
        device: u32,
    ) -> gpui_platform::GpuAdapterInfo {
        gpui_platform::GpuAdapterInfo {
            name: name.into(),
            device_type,
            vendor_id: vendor,
            device_id: device,
            backend: gpui_platform::GpuBackend::Vulkan,
            is_software: false,
        }
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn the_preference_orders_the_adapters_it_names() {
        use super::{CompositorGpuHint, adapter_key};
        use gpui_platform::{GpuDeviceType, GpuPreference};

        let integrated = facts("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916);
        let discrete = facts("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);
        let on_the_discrete = CompositorGpuHint {
            vendor_id: 0x10de,
            device_id: 0x1346,
        };

        assert!(
            adapter_key(&integrated, &GpuPreference::LowPower, Some(&on_the_discrete))
                < adapter_key(&discrete, &GpuPreference::LowPower, Some(&on_the_discrete)),
            "LowPower takes the integrated adapter even when the compositor is on the discrete one",
        );
        assert!(
            adapter_key(&discrete, &GpuPreference::HighPerformance, Some(&on_the_discrete))
                < adapter_key(&integrated, &GpuPreference::HighPerformance, Some(&on_the_discrete)),
            "HighPerformance takes the discrete adapter",
        );
        assert!(
            adapter_key(&discrete, &GpuPreference::Auto, None)
                < adapter_key(&integrated, &GpuPreference::Auto, None),
            "Auto keeps the historical discrete-first order",
        );
        assert!(
            adapter_key(
                &integrated,
                &GpuPreference::Auto,
                Some(&CompositorGpuHint {
                    vendor_id: 0x8086,
                    device_id: 0x1916,
                }),
            ) < adapter_key(
                &discrete,
                &GpuPreference::Auto,
                Some(&CompositorGpuHint {
                    vendor_id: 0x8086,
                    device_id: 0x1916,
                }),
            ),
            "Auto still follows the compositor's hint",
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_named_device_ranks_first_and_an_absent_one_falls_back() {
        use super::adapter_key;
        use gpui_platform::{GpuDeviceType, GpuId, GpuPreference};

        let integrated = facts("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916);
        let discrete = facts("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346);

        let named = GpuPreference::Device(GpuId {
            vendor_id: Some(0x8086),
            device_id: 0x1916,
        });
        assert!(adapter_key(&integrated, &named, None) < adapter_key(&discrete, &named, None));

        let absent = GpuPreference::Device(GpuId::by_device(0x9999));
        assert!(
            adapter_key(&discrete, &absent, None) < adapter_key(&integrated, &absent, None),
            "an absent device falls back to Auto's discrete-first order",
        );
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn a_custom_preference_chooses_one_adapter() {
        use super::custom_choice;
        use gpui_platform::{GpuAdapterInfo, GpuDeviceType};

        let adapters = [
            facts("integrated", GpuDeviceType::Integrated, 0x8086, 0x1916),
            facts("discrete", GpuDeviceType::Discrete, 0x10de, 0x1346),
        ];

        let choose_discrete =
            |gpus: &[GpuAdapterInfo]| gpus.iter().position(|gpu| gpu.device_id == 0x1346);
        assert_eq!(custom_choice(&adapters, &choose_discrete).unwrap(), 1);

        let chose_nothing = |_: &[GpuAdapterInfo]| None;
        assert!(
            custom_choice(&adapters, &chose_nothing).is_err(),
            "choosing no adapter is an error, not a silent fallback",
        );

        let out_of_range = |_: &[GpuAdapterInfo]| Some(2);
        let error =
            custom_choice(&adapters, &out_of_range).expect_err("an index past the list is refused");
        assert!(
            error.to_string().contains("only 2 adapter(s)"),
            "the refusal names the shortfall: {error}",
        );
    }
}
