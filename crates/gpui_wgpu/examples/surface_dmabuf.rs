//! Compose a dma-buf surface through the wgpu renderer and read the frame back.
//!
//! This is W3's exit criterion — *a dma-buf composites on a Linux host with an adapter* — exercised
//! without a compositor, for both shapes the surface arm takes: a single RGBA plane, and a two-plane
//! `NV12` buffer converted in the fragment shader.
//!
//! The producer is a bare Vulkan device (not wgpu): it allocates an exportable buffer, writes a known
//! pattern, and exports it as a linear dma-buf. The consumer is the same [`WgpuHeadlessRenderer`] a
//! window drives; it renders a [`Scene`] carrying a [`PaintSurface`] with that dma-buf and reads the
//! pixels back, through the very `import_dmabuf` and `draw_surfaces` code the window uses.
//!
//! Run it on Linux with a Vulkan adapter:
//!
//! ```text
//! cargo run -p gpui_wgpu --example surface_dmabuf --features test-support
//! ```
//!
//! It needs no display: the headless renderer renders offscreen and reads the target back.
//!
//! # Running this on a machine with two GPUs
//!
//! The headless renderer chooses an adapter itself, and on a laptop it may choose the discrete one.
//! A userspace fault *while GPU work is in flight* can wedge that GPU: an earlier revision of this
//! example dropped the Vulkan loader before the device it had made, and its crash produced
//! `NVRM: Xid 13` followed by repeated `Xid 158` (`NV_UFLUSH_FB_FLUSH` timeout) on an NVIDIA 930M,
//! hanging the machine until reboot. Keep the loader alive (this example does), and pin the adapter
//! to the integrated GPU, whose driver is well behaved here:
//!
//! ```text
//! ZED_DEVICE_ID=6422 cargo run -p gpui_wgpu --example surface_dmabuf --features test-support
//! ```
//!
//! `ZED_DEVICE_ID` is a PCI device id; 6422 is the Intel HD 520 on the machine this was written on.

#[cfg(all(target_os = "linux", feature = "test-support"))]
mod demo {
    use std::os::fd::{FromRawFd, OwnedFd};

    use anyhow::{Context as _, Result, ensure};
    use ash::vk;
    use gpui_engine::{
        DmaBufFormat, DmaBufHandle, DmaBufPlane, PaintSurface, Scene, SceneRenderer, SurfaceSource,
    };
    use gpui_platform::{Bounds, ContentMask, DevicePixels, Point, Size};
    use gpui_wgpu::WgpuHeadlessRenderer;

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

    /// Allocate a linear dma-buf holding `content`, and export its fd.
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

        let physical = unsafe { instance.enumerate_physical_devices() }
            .context("enumerate physical devices")?
            .into_iter()
            .next()
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

    /// Composite `handle` in a full-surface scene and return the centre pixel.
    fn composite(handle: DmaBufHandle) -> Result<Vec<u8>> {
        let bounds = Bounds {
            origin: Point {
                x: 0.0f32.into(),
                y: 0.0f32.into(),
            },
            size: Size {
                width: (WIDTH as f32).into(),
                height: (HEIGHT as f32).into(),
            },
        };
        let mut scene = Scene::default();
        scene.insert_primitive(PaintSurface {
            order: 0,
            bounds,
            content_mask: ContentMask { bounds },
            source: SurfaceSource::DmaBuf(handle),
        });
        scene.finish();

        let mut renderer = WgpuHeadlessRenderer::new().context("create the headless renderer")?;
        let image = renderer
            .render_scene_to_image(
                &scene,
                Size {
                    width: DevicePixels(WIDTH as i32),
                    height: DevicePixels(HEIGHT as i32),
                },
            )
            .context("render the scene")?;
        let offset = ((HEIGHT / 2) * WIDTH + (WIDTH / 2)) as usize * 4;
        Ok(image.data()[offset..offset + 4].to_vec())
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
        let expected = RGBA_COLOUR;
        println!("RGBA: producer wrote {expected:?}; composited {pixel:?}");
        ensure!(
            pixel == expected,
            "the composited pixel {pixel:?} does not match the producer's {expected:?}",
        );
        Ok(())
    }

    /// A two-plane `NV12` buffer: neutral chroma, so the shader's conversion must yield mid grey.
    fn nv12_case() -> Result<()> {
        let luma_size = (WIDTH * HEIGHT) as usize;
        let chroma_size = (WIDTH / 2 * HEIGHT / 2 * 2) as usize;
        let mut content = vec![0u8; luma_size + chroma_size];
        content[..luma_size].fill(128); // Y
        content[luma_size..].fill(128); // U/V, neutral

        let (_producer, fd) = produce_dmabuf(&content)?;
        let chroma_fd = fd.try_clone().context("duplicate the dma-buf for the chroma plane")?;
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
        println!("NV12: Y=U=V=128 converts to {expected:?}; composited {pixel:?}");
        for channel in 0..3 {
            let got = i32::from(pixel[channel]);
            let want = (expected[channel] * 255.0).round() as i32;
            ensure!(
                (got - want).abs() <= 2,
                "channel {channel} is {got}, but the shader's matrix predicts {want}",
            );
        }
        ensure!(pixel[3] == 255, "the surface should be opaque, but alpha is {}", pixel[3]);
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
        println!("OK — dma-buf surfaces composited through wgpu and read back as expected");
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
