//! Compose a dma-buf through the authoring `surface()` element and read the frame back.
//!
//! This is W3's exit criterion — *a dma-buf composites on a Linux host with an adapter* — end to end,
//! without a compositor. It is an ordinary GPUI app: a view whose `render` returns `surface(handle)`,
//! mounted in a real window through [`HeadlessAppContext`], laid out and drawn by the real renderer,
//! then captured with `capture_screenshot`. Nothing below reaches past the authoring API — the dma-buf
//! becomes a `SurfaceSource::DmaBuf` in the scene the way any element's paint does, and the renderer
//! composites it through the same `import_dmabuf` and `draw_surfaces` a window uses.
//!
//! The producer is a bare Vulkan device (not wgpu): it allocates an exportable buffer, writes a known
//! colour, and exports it as a linear dma-buf.
//!
//! Run it on Linux with a Vulkan adapter:
//!
//! ```text
//! cargo run -p gpui_wgpu --example surface_dmabuf --features test-support
//! ```
//!
//! # Running this on a machine with two GPUs
//!
//! The headless renderer chooses an adapter itself, and on a laptop it may choose the discrete one.
//! A userspace fault *while GPU work is in flight* can wedge that GPU: an earlier revision of this
//! example dropped the Vulkan loader before the device it had made, and its crash produced
//! `NVRM: Xid 13` followed by repeated `Xid 158` (`NV_UFLUSH_FB_FLUSH` timeout) on an NVIDIA 930M,
//! hanging the machine until reboot. The example keeps the loader alive and both the renderer and the
//! producer on the integrated GPU, but you can pin the adapter explicitly too:
//!
//! ```text
//! ZED_DEVICE_ID=1916 cargo run -p gpui_wgpu --example surface_dmabuf --features test-support
//! ```
//!
//! `ZED_DEVICE_ID` is a **four-digit hexadecimal** PCI device id; `1916` is the Intel HD 520 on the
//! machine this was written on (a decimal id like `6422` parses as the hex `0x6422`, matches nothing,
//! and silently falls back to the default adapter — the discrete one here).

#[cfg(all(target_os = "linux", feature = "test-support"))]
mod demo {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    use anyhow::{Context as _, Result, ensure};
    use ash::vk;
    use gpui::{
        AnyWindowHandle, AppContext as _, Context, DmaBufFormat, DmaBufHandle, DmaBufPlane,
        HeadlessAppContext, IntoElement, Render, Window, div, prelude::*, px, size, surface,
    };
    use gpui_wgpu::{CosmicTextSystem, WgpuHeadlessRenderer};

    const WIDTH: u32 = 64;
    const HEIGHT: u32 = 64;
    /// Opaque, so the surface's premultiplied-alpha blend leaves the colour unchanged.
    const RGBA_COLOUR: [u8; 4] = [32, 192, 64, 255];

    /// A minimal Vulkan "producer": the loader, instance, device and memory that keep the exported
    /// dma-buf alive for as long as the renderer samples it. The `Entry` must outlive the device it
    /// made: dropping it unloads the loader the device's calls go through.
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

    /// Allocate a linear dma-buf holding `content` on the integrated GPU, and export its fd.
    fn produce_dmabuf(content: &[u8]) -> Result<(Producer, OwnedFd)> {
        let entry = unsafe { ash::Entry::load() }
            .map_err(|error| anyhow::anyhow!("load vulkan: {error}"))?;
        let app_info = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app_info),
                None,
            )
        }
        .context("create the producer instance")?;

        // Prefer the integrated GPU, the same one the renderer is steered to: a fault on a discrete
        // GPU under the proprietary driver can wedge it, and nothing here needs a discrete GPU.
        let devices = unsafe { instance.enumerate_physical_devices() }
            .context("enumerate physical devices")?;
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
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
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
        }
        .context("allocate the producer buffer")?;

        unsafe {
            let mapped = device
                .map_memory(memory, 0, size, vk::MemoryMapFlags::empty())
                .context("map the producer buffer")? as *mut u8;
            std::ptr::copy_nonoverlapping(content.as_ptr(), mapped, content.len());
            device.unmap_memory(memory);
        }

        let fd = unsafe {
            external_memory_fd.get_memory_fd(
                &vk::MemoryGetFdInfoKHR::default()
                    .memory(memory)
                    .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT),
            )
        }
        .context("export the producer's dma-buf")?;
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

    /// The app: a view that paints the dma-buf as a surface filling the window.
    struct SurfaceDemo {
        handle: DmaBufHandle,
    }

    impl Render for SurfaceDemo {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut Context<Self>,
        ) -> impl IntoElement {
            div()
                .size_full()
                .child(surface(self.handle.clone()).w_full().h_full())
        }
    }

    /// Open a window with the surface, draw a frame, and read the centre pixel back.
    fn composite(handle: DmaBufHandle) -> Result<[u8; 4]> {
        let text_system = Arc::new(CosmicTextSystem::new("fallback"));
        // The Linux platform has no headless renderer of its own (unlike macOS's Metal one), so the
        // example supplies the wgpu one — the same renderer a window uses.
        let mut cx = HeadlessAppContext::with_platform(text_system, Arc::new(()), || {
            Ok(Some(
                Box::new(WgpuHeadlessRenderer::new()?) as Box<dyn gpui::SceneRenderer>
            ))
        });

        let window = cx.open_window(size(px(WIDTH as f32), px(HEIGHT as f32)), |_window, cx| {
            cx.new(|_| SurfaceDemo { handle })
        })?;
        let window: AnyWindowHandle = window.into();

        // A view renders on the next frame; force one so `rendered_frame` holds our surface.
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| {
            let _ = window.draw(cx);
        })?;

        let image = cx.capture_screenshot(window)?;
        let pixel = image.get_pixel(WIDTH / 2, HEIGHT / 2).0;
        Ok(pixel)
    }

    /// A single RGBA plane: the producer's bytes must survive unchanged.
    fn rgba_case() -> Result<()> {
        let mut content = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for _ in 0..(WIDTH * HEIGHT) {
            content.extend_from_slice(&RGBA_COLOUR);
        }
        let (_producer, fd) = produce_dmabuf(&content)?;
        let handle = DmaBufHandle::new(
            WIDTH,
            HEIGHT,
            DmaBufFormat::Rgba8,
            DmaBufHandle::LINEAR,
            [DmaBufPlane::new(fd, 0, WIDTH * 4)],
            None,
        );

        let pixel = composite(handle)?;
        println!("RGBA: producer wrote {RGBA_COLOUR:?}; the composited pixel is {pixel:?}");
        ensure!(
            pixel == RGBA_COLOUR,
            "the composited pixel {pixel:?} does not match the producer's {RGBA_COLOUR:?}",
        );
        Ok(())
    }

    /// A two-plane `NV12` buffer: neutral chroma, so the shader's conversion must yield mid grey.
    fn nv12_case() -> Result<()> {
        let luma_size = (WIDTH * HEIGHT) as usize;
        let chroma_size = (WIDTH / 2 * HEIGHT / 2 * 2) as usize;
        let content = vec![128u8; luma_size + chroma_size];

        let (_producer, fd) = produce_dmabuf(&content)?;
        let chroma_fd = fd
            .try_clone()
            .context("duplicate the dma-buf for the chroma plane")?;
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

        let pixel = composite(handle)?;
        // The same BT.601 matrix the shader applies, on the CPU, for Y=U=V=128/255.
        let expected = ycbcr_to_rgb(128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0);
        println!("NV12: Y=U=V=128 converts to {expected:?}; the composited pixel is {pixel:?}");
        for channel in 0..3 {
            let got = i32::from(pixel[channel]);
            let want = (expected[channel] * 255.0).round() as i32;
            ensure!(
                (got - want).abs() <= 2,
                "channel {channel} is {got}, but the shader's matrix predicts {want}",
            );
        }
        ensure!(
            pixel[3] == 255,
            "the surface should be opaque, but alpha is {}",
            pixel[3],
        );
        Ok(())
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

    pub fn run() -> Result<()> {
        rgba_case()?;
        nv12_case()?;
        println!("OK — dma-buf surfaces composited through the authoring API and read back as expected");
        Ok(())
    }
}

#[cfg(all(target_os = "linux", feature = "test-support"))]
fn main() -> anyhow::Result<()> {
    demo::run()
}

#[cfg(not(all(target_os = "linux", feature = "test-support")))]
fn main() {
    eprintln!(
        "surface_dmabuf is a Linux example and needs the test-support feature: \
         cargo run -p gpui_wgpu --example surface_dmabuf --features test-support"
    );
}
