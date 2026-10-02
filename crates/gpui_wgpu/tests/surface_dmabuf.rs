//! A dma-buf composited through the authoring `surface()` element, end to end.
//!
//! The demo (`examples/surface_dmabuf.rs`) shows this by hand; this asserts it. The producer is a
//! bare Vulkan device (not wgpu), the consumer is the platform's headless renderer — the same one a
//! window drives — and the pixels are read back with `capture_screenshot`.
//!
//! Opt in, because it needs a GPU and the crate's own unit-test target does not build:
//!
//! ```text
//! ZED_DEVICE_ID=1916 cargo test -p gpui_wgpu --features test-support --test surface_dmabuf
//! ```
//!
//! It skips (rather than fails) where there is no Vulkan device, and it pins the adapter the same way
//! the demo does when you pass `ZED_DEVICE_ID`.

#![cfg(all(target_os = "linux", feature = "test-support"))]

use std::io::Write as _;
use std::os::fd::{FromRawFd, OwnedFd};

use ash::vk;
use gpui::{
    AnyWindowHandle, AppContext as _, Context, DmaBufFormat, DmaBufHandle, DmaBufPlane,
    HeadlessAppContext, IntoElement, Render, Window, div, prelude::*, px, size, surface,
};
use gpui_wgpu::CosmicTextSystem;

const WIDTH: u32 = 64;
const HEIGHT: u32 = 64;
/// Opaque, so the surface's premultiplied-alpha blend leaves the colour unchanged.
const RGBA_COLOUR: [u8; 4] = [32, 192, 64, 255];

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
        content.extend_from_slice(&RGBA_COLOUR);
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
    assert_eq!(pixel, RGBA_COLOUR);
}

#[test]
fn a_surface_with_a_signalled_acquire_fence_is_waited_on() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    let mut content = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for _ in 0..(WIDTH * HEIGHT) {
        content.extend_from_slice(&RGBA_COLOUR);
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
    assert_eq!(pixel, RGBA_COLOUR);
}

#[test]
fn an_nv12_surface_converts_through_the_shader() {
    if !gpu_available() {
        eprintln!("skipping: no Vulkan adapter for the headless renderer");
        return;
    }
    let luma_size = (WIDTH * HEIGHT) as usize;
    let chroma_size = (WIDTH / 2 * HEIGHT / 2 * 2) as usize;
    // Neutral chroma, so the shader's conversion must yield mid grey.
    let content = vec![128u8; luma_size + chroma_size];
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
    // The same BT.601 matrix the shader applies, on the CPU, for Y=U=V=128/255.
    let expected = ycbcr_to_rgb(128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0);
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
