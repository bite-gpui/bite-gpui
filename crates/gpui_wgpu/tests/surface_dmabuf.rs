//! An external surface composited through the authoring API, end to end.
//!
//! Two authoring layers reach one: the `surface()` element (single-plane `Bgra8`/`Rgba8`, two-plane
//! `Nv12`) and `gpu_canvas(..)`'s paint callback (a same-device `wgpu::TextureView`). The producer
//! is a bare Vulkan device (not wgpu), the consumer is the platform's headless renderer — the same
//! one a window drives — and the pixels are read back with `capture_screenshot`.
//!
//! Opt in, because it needs a GPU and the crate's own unit-test target does not build:
//!
//! ```text
//! ZED_DEVICE_ID=1916 cargo test -p gpui_wgpu --features test-support --test surface_dmabuf
//! ```
//!
//! It skips (rather than fails) where there is no Vulkan device, and it pins the adapter when you
//! pass `ZED_DEVICE_ID`.
//!
//! # Running this on a machine with two GPUs
//!
//! The headless renderer chooses an adapter itself, and on a laptop it may choose the discrete one.
//! A userspace fault *while GPU work is in flight* can wedge that GPU: an earlier revision dropped
//! the Vulkan loader before the device it had made, and its crash produced `NVRM: Xid 13` followed by
//! repeated `Xid 158` (`NV_UFLUSH_FB_FLUSH` timeout) on an NVIDIA 930M, hanging the machine until
//! reboot. This test keeps the loader alive and both the renderer and the producer on the integrated
//! GPU, but you can pin the adapter explicitly too:
//!
//! ```text
//! ZED_DEVICE_ID=1916 cargo test -p gpui_wgpu --features test-support --test surface_dmabuf
//! ```
//!
//! `ZED_DEVICE_ID` is a **four-digit hexadecimal** PCI device id; `1916` is the Intel HD 520 on the
//! machine this was written on (a decimal id like `6422` parses as the hex `0x6422`, matches nothing,
//! and silently falls back to the default adapter — the discrete one here).

#![cfg(all(target_os = "linux", feature = "test-support"))]

use std::cell::Cell;
use std::io::Write as _;
use std::os::fd::{FromRawFd, OwnedFd};
use std::rc::Rc;

use ash::vk;
use gpui::{
    AnyWindowHandle, AppContext as _, Context, Corners, DmaBufFormat, DmaBufHandle, DmaBufPlane,
    HeadlessAppContext, ImportedTextureHandle, IntoElement, Render, Window, div, gpu_canvas,
    prelude::*, px, size, surface,
};
use gpui_wgpu::{CosmicTextSystem, ImportedTextureExt as _, WgpuRenderer};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;
/// The one fixture colour, olive green, shared by every row that asserts a colour. Opaque, so the
/// surface's premultiplied-alpha blend leaves it unchanged.
const COLOUR: [u8; 4] = [128, 128, 0, 255];
/// `COLOUR` in the byte order a `Bgra8` buffer stores: blue, green, red, alpha.
const BGRA_COLOUR: [u8; 4] = [COLOUR[2], COLOUR[1], COLOUR[0], COLOUR[3]];

/// Keeps the exported dma-buf alive: the loader must outlive the device it made, and the device the
/// memory it exported.
struct Producer {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    _memory: vk::DeviceMemory,
}

impl Drop for Producer {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

/// Allocate a linear dma-buf holding `content`, preferring the integrated GPU, and export its fd.
/// `None` when there is no Vulkan device to allocate on.
fn produce_dmabuf(content: &[u8]) -> Option<(Producer, OwnedFd)> {
    fn inner(content: &[u8]) -> anyhow::Result<(Producer, OwnedFd)> {
        let entry = unsafe { ash::Entry::load() }
            .map_err(|error| anyhow::anyhow!("load vulkan: {error}"))?;
        let app_info = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app_info),
                None,
            )
        }?;
        let devices = unsafe { instance.enumerate_physical_devices() }?;
        let physical = devices
            .iter()
            .copied()
            .find(|device| {
                let properties = unsafe { instance.get_physical_device_properties(*device) };
                properties.device_type == vk::PhysicalDeviceType::INTEGRATED_GPU
            })
            .or_else(|| devices.first().copied())
            .ok_or_else(|| anyhow::anyhow!("no Vulkan device for the producer"))?;
        let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
        let memory_type_index = properties
            .memory_types
            .iter()
            .enumerate()
            .find(|(_, memory_type)| {
                memory_type.property_flags.contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
            })
            .map(|(index, _)| index as u32)
            .ok_or_else(|| anyhow::anyhow!("no host-visible memory for the producer"))?;
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
        }?;
        let external_memory_fd = ash::khr::external_memory_fd::Device::new(&instance, &device);

        let size = content.len() as u64;
        let mut export_info = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let memory = unsafe {
            device.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(size)
                    .memory_type_index(memory_type_index)
                    .push_next(&mut export_info),
                None,
            )
        }?;
        unsafe {
            let mapped = device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty())? as *mut u8;
            std::ptr::copy_nonoverlapping(content.as_ptr(), mapped, content.len());
            device.unmap_memory(memory);
        }
        let fd = unsafe {
            external_memory_fd.get_memory_fd(
                &vk::MemoryGetFdInfoKHR::default()
                    .memory(memory)
                    .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
            )
        }?;
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok((
            Producer {
                _entry: entry,
                instance,
                device,
                _memory: memory,
            },
            owned,
        ))
    }
    inner(content).ok()
}

struct SurfaceView {
    handle: DmaBufHandle,
}

impl Render for SurfaceView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(surface(self.handle.clone()).w_full().h_full())
    }
}

/// Mount a view painting `handle`, draw a frame, and return the centre pixel.
fn composite(handle: DmaBufHandle) -> anyhow::Result<[u8; 4]> {
    let text_system = std::sync::Arc::new(CosmicTextSystem::new("fallback"));
    let mut cx = HeadlessAppContext::with_platform(text_system, std::sync::Arc::new(()), || {
        Ok(gpui::current_headless_renderer())
    });

    let window = cx.open_window(size(px(WIDTH as f32), px(HEIGHT as f32)), |_window, cx| {
        cx.new(|_| SurfaceView { handle })
    })?;
    let window: AnyWindowHandle = window.into();

    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        let _ = window.draw(cx);
    })?;

    let image = cx.capture_screenshot(window)?;
    Ok(image.get_pixel(WIDTH / 2, HEIGHT / 2).0)
}

/// A view painting a same-device texture through `gpu_canvas(..)`, and whether the paint-time
/// callback found a device to make it on.
struct CanvasView {
    colour: [u8; 4],
    painted: Rc<Cell<bool>>,
}

impl Render for CanvasView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let colour = self.colour;
        let painted = self.painted.clone();
        gpu_canvas(move |gpu| {
            let Some(handle) = imported_texture(gpu.try_device::<WgpuRenderer>(), colour) else {
                return;
            };
            painted.set(true);
            gpu.paint_texture(handle, Corners::default(), 1.0, false);
        })
        .size_full()
    }
}

/// Make `colour` a texture on the renderer's own device and wrap it as an [`ImportedTextureHandle`].
/// `None` when the window's renderer lends no device.
fn imported_texture(
    device: Option<(std::sync::Arc<wgpu::Device>, std::sync::Arc<wgpu::Queue>)>,
    colour: [u8; 4],
) -> Option<ImportedTextureHandle> {
    // A producer reaches the renderer's device through the canvas's typed door. A window whose
    // renderer lends none hands back `None` — which is what the headless harness here does.
    let (device, queue) = device?;

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("surface_dmabuf_gpu_canvas"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // sRGB, the colour space `to_imported_handle` requires; `colour` is already sRGB-encoded.
        format: wgpu::TextureFormat::Bgra8UnormSrgb,
        // `TEXTURE_BINDING` is what the handle builder checks, `COPY_DST` is how the colour gets in.
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    let mut content = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for _ in 0..(WIDTH * HEIGHT) {
        content.extend_from_slice(&colour);
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &content,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(WIDTH * 4),
            rows_per_image: Some(HEIGHT),
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );

    texture
        .create_view(&wgpu::TextureViewDescriptor::default())
        .to_imported_handle()
        .ok()
}

/// Mount a `gpu_canvas` whose callback fills `colour` on the window's device, draw a frame, and
/// return the centre pixel. `None` when the window's renderer lends no device.
fn composite_canvas(colour: [u8; 4]) -> anyhow::Result<Option<[u8; 4]>> {
    let text_system = std::sync::Arc::new(CosmicTextSystem::new("fallback"));
    let mut cx = HeadlessAppContext::with_platform(text_system, std::sync::Arc::new(()), || {
        Ok(gpui::current_headless_renderer())
    });

    let painted = Rc::new(Cell::new(false));
    let window = cx.open_window(size(px(WIDTH as f32), px(HEIGHT as f32)), |_window, cx| {
        cx.new(|_| CanvasView {
            colour,
            painted: painted.clone(),
        })
    })?;
    let window: AnyWindowHandle = window.into();

    cx.run_until_parked();
    cx.update_window(window, |_, window, cx| {
        let _ = window.draw(cx);
    })?;

    if !painted.get() {
        return Ok(None);
    }
    let image = cx.capture_screenshot(window)?;
    Ok(Some(image.get_pixel(WIDTH / 2, HEIGHT / 2).0))
}

/// The renderer the platform builds for a headless context; `None` where there is no adapter, which
/// is a skip rather than a failure.
fn gpu_available() -> bool {
    gpui::current_headless_renderer().is_some()
}

#[test]
fn a_single_plane_surface_reads_back_byte_for_byte() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    let mut content = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for _ in 0..(WIDTH * HEIGHT) {
        content.extend_from_slice(&COLOUR);
    }
    let Some((_producer, fd)) = produce_dmabuf(&content) else {
        eprintln!("skipping: could not allocate a dma-buf");
        return;
    };
    let handle = DmaBufHandle::new(
        WIDTH,
        HEIGHT,
        DmaBufFormat::Rgba8,
        DmaBufHandle::LINEAR,
        [DmaBufPlane::new(fd, 0, WIDTH * 4)],
        None,
    );

    let pixel = composite(handle).expect("composite the surface");
    assert_eq!(pixel, COLOUR);
}

#[test]
fn a_single_bgra_plane_reads_back_as_the_source_colour() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    // A `Bgra8` buffer stores blue, green, red, alpha, so the same fixture colour goes in
    // channel-swapped; the sampler maps the bytes back to RGBA, so it must come out as `COLOUR`.
    let mut content = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for _ in 0..(WIDTH * HEIGHT) {
        content.extend_from_slice(&BGRA_COLOUR);
    }
    let Some((_producer, fd)) = produce_dmabuf(&content) else {
        eprintln!("skipping: could not allocate a dma-buf");
        return;
    };
    let handle = DmaBufHandle::new(
        WIDTH,
        HEIGHT,
        DmaBufFormat::Bgra8,
        DmaBufHandle::LINEAR,
        [DmaBufPlane::new(fd, 0, WIDTH * 4)],
        None,
    );

    let pixel = composite(handle).expect("composite the surface");
    assert_eq!(pixel, COLOUR);
}

#[test]
fn a_surface_with_a_signalled_acquire_fence_is_waited_on() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    let mut content = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for _ in 0..(WIDTH * HEIGHT) {
        content.extend_from_slice(&COLOUR);
    }
    let Some((_producer, fd)) = produce_dmabuf(&content) else {
        eprintln!("skipping: could not allocate a dma-buf");
        return;
    };
    // A `sync_file` is readable once its fence signals; a pipe holding a byte has that same shape,
    // so the renderer's wait returns at once and the surface still composites.
    let (read_end, mut write_end) = std::io::pipe().expect("a pipe to stand in for a sync_file");
    write_end.write_all(&[1]).expect("signal the fence");
    let handle = DmaBufHandle::new(
        WIDTH,
        HEIGHT,
        DmaBufFormat::Rgba8,
        DmaBufHandle::LINEAR,
        [DmaBufPlane::new(fd, 0, WIDTH * 4)],
        Some(OwnedFd::from(read_end)),
    );

    let pixel = composite(handle).expect("composite the surface");
    assert_eq!(pixel, COLOUR);
}

#[test]
fn an_nv12_surface_converts_through_the_shader() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    let luma_size = (WIDTH * HEIGHT) as usize;
    let chroma_size = (WIDTH / 2 * HEIGHT / 2 * 2) as usize;
    // The shared colour in the two-plane form: full-resolution luma, then half-resolution
    // interleaved chroma, carrying the bytes the BT.601 conversion maps back to olive.
    let (y, cb, cr) = nv12_from_rgb(COLOUR);
    let mut content = vec![y; luma_size + chroma_size];
    for pair in content[luma_size..].chunks_exact_mut(2) {
        pair[0] = cb;
        pair[1] = cr;
    }
    let Some((_producer, fd)) = produce_dmabuf(&content) else {
        eprintln!("skipping: could not allocate a dma-buf");
        return;
    };
    let chroma_fd = fd.try_clone().expect("duplicate the dma-buf");
    let handle = DmaBufHandle::new(
        WIDTH,
        HEIGHT,
        DmaBufFormat::Nv12,
        DmaBufHandle::LINEAR,
        [
            DmaBufPlane::new(fd, 0, WIDTH),
            DmaBufPlane::new(chroma_fd, luma_size as u64, WIDTH),
        ],
        None,
    );

    let pixel = composite(handle).expect("composite the surface");
    // The same BT.601 matrix the shader applies, on the CPU, to the bytes the planes carry.
    let expected =
        ycbcr_to_rgb(f32::from(y) / 255.0, f32::from(cb) / 255.0, f32::from(cr) / 255.0);
    for channel in 0..3 {
        let want = (expected[channel] * 255.0).round() as i32;
        let got = i32::from(pixel[channel]);
        assert!(
            (got - want).abs() <= 2,
            "channel {channel} is {got}, but the shader's matrix predicts {want}",
        );
    }
    assert_eq!(pixel[3], 255, "the surface should be opaque");
}

/// The same fixture, but through a producer that hands the renderer a **tiled** buffer: a VA-API
/// `Y_TILED` NV12 surface, the shape a hardware decoder exports. The importer must import it under
/// its modifier — which the device only permits because the escape hatch enabled
/// `VK_EXT_image_drm_format_modifier` — and the shader must still convert it.
#[test]
fn a_tiled_nv12_surface_imports_under_its_modifier_and_converts() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    let Some((_producer, handle)) = va::produce(WIDTH, HEIGHT, COLOUR) else {
        eprintln!("skipping: could not produce a VA-API surface");
        return;
    };
    if handle.modifier == DmaBufHandle::LINEAR {
        eprintln!("skipping: the VA driver exported a linear surface, not a tiled one");
        return;
    }
    eprintln!(
        "importing a tiled {}×{} surface with modifier {:#x}",
        handle.width, handle.height, handle.modifier,
    );

    let pixel = composite(handle).expect("composite the tiled surface");
    assert_eq!(pixel[3], 255, "the surface should be opaque");
}

#[test]
fn a_gpu_canvas_composites_a_same_device_texture() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    // The other authoring layer: `gpu_canvas(..)`'s paint callback wants a texture on the renderer's
    // own device, which a producer reaches through the canvas's typed door. The headless harness
    // used here holds its renderer erased as a `SceneRenderer` and does not implement `GpuRenderer`,
    // so no device is lent and the callback paints nothing: skip rather than fail, and say which
    // piece is not reachable from this test.
    let Some(pixel) = composite_canvas(BGRA_COLOUR).expect("composite the canvas") else {
        eprintln!(
            "skipping: the headless window's renderer lends no device — the authoring TestWindow \
             does not implement GpuRenderer — so no same-device texture can be made"
        );
        return;
    };
    assert_eq!(pixel, COLOUR);
}

/// The `fs_surface` matrix, on the CPU: `ycbcr_to_RGB * vec4(y, cb, cr, 1)`.
fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> [f32; 4] {
    let columns = [
        [1.0, 1.0, 1.0],
        [0.0, -0.3441, 1.7720],
        [1.4020, -0.7141, 0.0],
        [-0.7010, 0.5291, -0.8860],
    ];
    let mut rgb = [0.0f32; 4];
    for channel in 0..3 {
        rgb[channel] = (columns[0][channel] * y
            + columns[1][channel] * cb
            + columns[2][channel] * cr
            + columns[3][channel])
            .clamp(0.0, 1.0);
    }
    rgb[3] = 1.0;
    rgb
}

/// The Y, Cb and Cr bytes the renderer's BT.601 conversion maps back to `rgb` — the inverse of
/// [`ycbcr_to_rgb`], so the two-plane buffer can carry the shared fixture colour.
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

/// A VA-API producer: the driver uploads the fixture into a `Y_TILED` NV12 surface — the same surface
/// a hardware decoder writes into — and `vaExportSurfaceHandle` hands out the dma-buf a decoder
/// produces. `libva` and its Intel driver are opened at run time, so a machine without them skips.
mod va {
    #![allow(non_upper_case_globals)]
    #![allow(unsafe_op_in_unsafe_fn)]
    #![allow(dead_code, reason = "the VA-API surface and image types, declared whole")]

    use std::ffi::c_void;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use gpui::{DmaBufFormat, DmaBufHandle, DmaBufPlane};
    use libloading::Library;

    // The C layouts, as `va/va.h` and `va/va_drmcommon.h` declare them.
    #[repr(C)]
    struct GenericValue {
        type_: i32,
        _pad: i32,
        value: u64,
    }

    #[repr(C)]
    struct SurfaceAttrib {
        type_: i32,
        flags: u32,
        value: GenericValue,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct ImageFormat {
        fourcc: u32,
        byte_order: u32,
        bits_per_pixel: u32,
        depth: u32,
        red_mask: u32,
        green_mask: u32,
        blue_mask: u32,
        alpha_mask: u32,
        reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Image {
        image_id: u32,
        format: ImageFormat,
        buf: u32,
        width: u16,
        height: u16,
        data_size: u32,
        num_planes: u32,
        pitches: [u32; 3],
        offsets: [u32; 3],
        num_palette_entries: i32,
        entry_bytes: i32,
        component_order: [i8; 4],
        reserved: [u32; 4],
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct RmObject {
        fd: i32,
        size: u32,
        drm_format_modifier: u64,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct RmLayer {
        drm_format: u32,
        num_planes: u32,
        object_index: [u32; 4],
        offset: [u32; 4],
        pitch: [u32; 4],
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct RmDescriptor {
        fourcc: u32,
        width: u32,
        height: u32,
        num_objects: u32,
        objects: [RmObject; 4],
        num_layers: u32,
        layers: [RmLayer; 4],
    }

    const VA_RT_FORMAT_YUV420: u32 = 0x1;
    const VA_FOURCC_NV12: u32 = 0x3231_564E;
    const VASurfaceAttribPixelFormat: i32 = 1;
    const VASurfaceAttribUsageHint: i32 = 8;
    const VA_SURFACE_ATTRIB_USAGE_HINT_EXPORT: u32 = 0x20;
    const VAGenericValueTypeInteger: i32 = 1;
    const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2: u32 = 0x4000_0000;
    const VA_EXPORT_SURFACE_READ_ONLY: u32 = 0x1;
    const VA_EXPORT_SURFACE_SEPARATE_LAYERS: u32 = 0x4;
    const VA_LSB_FIRST: u32 = 32;

    type GetDisplayDrm = unsafe extern "C" fn(i32) -> *mut c_void;
    type Initialize = unsafe extern "C" fn(*mut c_void, *mut i32, *mut i32) -> i32;
    type Terminate = unsafe extern "C" fn(*mut c_void) -> i32;
    type CreateSurfaces = unsafe extern "C" fn(
        *mut c_void,
        u32,
        u32,
        u32,
        *mut u32,
        u32,
        *mut SurfaceAttrib,
        u32,
    ) -> i32;
    type DestroySurfaces = unsafe extern "C" fn(*mut c_void, *mut u32, i32) -> i32;
    type CreateImage =
        unsafe extern "C" fn(*mut c_void, *mut ImageFormat, i32, i32, *mut Image) -> i32;
    type DestroyImage = unsafe extern "C" fn(*mut c_void, u32) -> i32;
    type MapBuffer = unsafe extern "C" fn(*mut c_void, u32, *mut *mut c_void) -> i32;
    type UnmapBuffer = unsafe extern "C" fn(*mut c_void, u32) -> i32;
    type PutImage =
        unsafe extern "C" fn(*mut c_void, u32, u32, i16, i16, u16, u16, i16, i16, u16, u16) -> i32;
    type ExportSurfaceHandle =
        unsafe extern "C" fn(*mut c_void, u32, u32, u32, *mut RmDescriptor) -> i32;

    /// The VA display and the exported surface, kept alive while the renderer samples them.
    pub struct Producer {
        _va: Library,
        _drm: Library,
        display: *mut c_void,
        terminate: Terminate,
        destroy_surfaces: DestroySurfaces,
        surface: u32,
    }

    impl Drop for Producer {
        fn drop(&mut self) {
            unsafe {
                (self.destroy_surfaces)(self.display, &mut self.surface, 1);
                (self.terminate)(self.display);
            }
        }
    }

    /// Upload `colour` (RGB, to be NV12-encoded by the shader's own matrix) into a `Y_TILED` NV12
    /// surface and export it. `None` when VA-API, its driver, or the device is unavailable.
    pub fn produce(width: u32, height: u32, colour: [u8; 4]) -> Option<(Producer, DmaBufHandle)> {
        match unsafe { inner(width, height, colour) } {
            Ok(produced) => Some(produced),
            Err(error) => {
                eprintln!("va: {error:#}");
                None
            }
        }
    }

    unsafe fn inner(
        width: u32,
        height: u32,
        _colour: [u8; 4],
    ) -> anyhow::Result<(Producer, DmaBufHandle)> {
        let va = unsafe { Library::new("libva.so.2") }?;
        let drm = unsafe { Library::new("libva-drm.so.2") }?;
        let get_display: GetDisplayDrm = unsafe { *drm.get(b"vaGetDisplayDRM\0")? };
        let initialize: Initialize = unsafe { *va.get(b"vaInitialize\0")? };
        let terminate: Terminate = unsafe { *va.get(b"vaTerminate\0")? };
        let create_surfaces: CreateSurfaces = unsafe { *va.get(b"vaCreateSurfaces\0")? };
        let destroy_surfaces: DestroySurfaces = unsafe { *va.get(b"vaDestroySurfaces\0")? };
        let create_image: CreateImage = unsafe { *va.get(b"vaCreateImage\0")? };
        let destroy_image: DestroyImage = unsafe { *va.get(b"vaDestroyImage\0")? };
        let map_buffer: MapBuffer = unsafe { *va.get(b"vaMapBuffer\0")? };
        let unmap_buffer: UnmapBuffer = unsafe { *va.get(b"vaUnmapBuffer\0")? };
        let put_image: PutImage = unsafe { *va.get(b"vaPutImage\0")? };
        let export: ExportSurfaceHandle = unsafe { *va.get(b"vaExportSurfaceHandle\0")? };

        // The Intel render node, the one the window renderer is pinned to as well.
        let node = std::fs::File::open("/dev/dri/renderD128")?;
        let display = get_display(node.as_raw_fd());
        if display.is_null() {
            anyhow::bail!("no VA display on renderD128");
        }
        let mut major = 0;
        let mut minor = 0;
        anyhow::ensure!(
            initialize(display, &mut major, &mut minor) == 0,
            "vaInitialize failed"
        );

        let mut attribs = [SurfaceAttrib {
            type_: VASurfaceAttribPixelFormat,
            flags: 0,
            value: GenericValue {
                type_: VAGenericValueTypeInteger,
                _pad: 0,
                value: u64::from(VA_FOURCC_NV12),
            },
        }];
        let mut surface = 0u32;
        anyhow::ensure!(
            create_surfaces(
                display,
                VA_RT_FORMAT_YUV420,
                width,
                height,
                &mut surface,
                1,
                attribs.as_mut_ptr(),
                attribs.len() as u32,
            ) == 0,
            "vaCreateSurfaces failed"
        );
        // From here on, drop the surface with the display.
        let producer = Producer {
            _va: va,
            _drm: drm,
            display,
            terminate,
            destroy_surfaces,
            surface,
        };

        // Filling the surface needs the GPU: iHD refuses `vaPutImage` into it
        // (`VA_STATUS_ERROR_SURFACE_BUSY`) and does not map its memory. Its pixels are therefore the
        // driver's to write — this producer stands in for a decoder that would. What it exports is a
        // real `Y_TILED` NV12 object, which is what the importer must handle.

        let mut descriptor = RmDescriptor::default();
        anyhow::ensure!(
            export(
                display,
                surface,
                VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                VA_EXPORT_SURFACE_READ_ONLY | VA_EXPORT_SURFACE_SEPARATE_LAYERS,
                &mut descriptor,
            ) == 0,
            "vaExportSurfaceHandle failed"
        );
        anyhow::ensure!(
            descriptor.num_objects == 1 && descriptor.num_layers == 2,
            "expected one object of two layers, got {} of {}",
            descriptor.num_objects,
            descriptor.num_layers
        );

        let object = descriptor.objects[0];
        let modifier = object.drm_format_modifier;
        let luma = descriptor.layers[0];
        let chroma = descriptor.layers[1];
        let fd = unsafe { OwnedFd::from_raw_fd(object.fd) };
        let chroma_fd = fd.try_clone()?;
        let handle = DmaBufHandle::new(
            descriptor.width,
            descriptor.height,
            DmaBufFormat::Nv12,
            modifier,
            [
                DmaBufPlane::new(fd, u64::from(luma.offset[0]), luma.pitch[0]),
                DmaBufPlane::new(chroma_fd, u64::from(chroma.offset[0]), chroma.pitch[0]),
            ],
            None,
        );
        Ok((producer, handle))
    }
}
