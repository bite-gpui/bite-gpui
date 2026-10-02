//! Importing a dma-buf into `wgpu` textures, on the Vulkan backend.
//!
//! `wgpu` can adopt a raw `VkImage` (`texture_from_raw`) but cannot create one over an external
//! allocation, so the import is done with `ash` against the very device `wgpu` draws on. The recipe
//! is the one the P3 probe proved (`bite-gpui`/`probes/linux-dmabuf`): create the image, import the
//! descriptor into a **dedicated** allocation, bind it, and hand the image to `wgpu`. The dedicated
//! allocation is not decoration — the probe found the discrete GPU refusing the import on every
//! memory type without it, while still advertising the format `IMPORTABLE`.
//!
//! A descriptor is consumed by the import (the driver takes ownership), so each plane imports a
//! `dup` of its descriptor; that is also what lets an `NV12` buffer's two planes, which may share one
//! descriptor, become two independently-owned textures.
//!
//! **Linear only for now.** The probe showed a tiled import needs `VK_EXT_image_drm_format_modifier`
//! enabled on this device and an explicit plane layout; a non-linear modifier is refused with a
//! message rather than sampled wrong.

use std::os::fd::IntoRawFd;

use anyhow::{Context as _, Result};
use ash::vk;
use gpui_engine::{DmaBufFormat, DmaBufHandle, DmaBufPlane};

/// A plane's texture, adopted into `wgpu`.
pub(crate) struct PlaneTexture {
    /// The wgpu texture `wgpu` owns, over the imported image.
    pub texture: wgpu::Texture,
}

/// Import a dma-buf as the texture(s) the surface pipeline composites.
///
/// One texture for `Bgra8`/`Rgba8`, two (`R8` then `Rg8`) for `NV12`.
pub(crate) fn import_dmabuf(
    device: &wgpu::Device,
    handle: &DmaBufHandle,
) -> Result<Vec<PlaneTexture>> {
    anyhow::ensure!(
        handle.modifier == DmaBufHandle::LINEAR,
        "gpui_wgpu imports linear dma-bufs only, but this buffer declares modifier {:#x}; \
         a producer sharing across GPUs must use DRM_FORMAT_MOD_LINEAR",
        handle.modifier,
    );
    let expected = match handle.format {
        DmaBufFormat::Bgra8 | DmaBufFormat::Rgba8 => 1,
        DmaBufFormat::Nv12 => 2,
    };
    anyhow::ensure!(
        handle.planes.len() == expected,
        "{:?} needs {expected} plane(s), but the handle carries {}",
        handle.format,
        handle.planes.len(),
    );

    let hal = unsafe { device.as_hal::<wgpu::hal::vulkan::Api>() }
        .context("the dma-buf surface arm needs the Vulkan backend")?;
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();

    let mut textures = Vec::with_capacity(expected);
    match handle.format {
        DmaBufFormat::Bgra8 | DmaBufFormat::Rgba8 => {
            let (wgpu_format, vk_format) = match handle.format {
                DmaBufFormat::Bgra8 => (wgpu::TextureFormat::Bgra8Unorm, vk::Format::B8G8R8A8_UNORM),
                _ => (wgpu::TextureFormat::Rgba8Unorm, vk::Format::R8G8B8A8_UNORM),
            };
            textures.push(import_plane(
                device,
                &*hal,
                instance,
                physical,
                &handle.planes[0],
                vk_format,
                wgpu_format,
                handle.width,
                handle.height,
            )?);
        }
        DmaBufFormat::Nv12 => {
            // Plane 0 is luma at full resolution; plane 1 is interleaved chroma at half, in both
            // dimensions.
            textures.push(import_plane(
                device,
                &*hal,
                instance,
                physical,
                &handle.planes[0],
                vk::Format::R8_UNORM,
                wgpu::TextureFormat::R8Unorm,
                handle.width,
                handle.height,
            )?);
            textures.push(import_plane(
                device,
                &*hal,
                instance,
                physical,
                &handle.planes[1],
                vk::Format::R8G8_UNORM,
                wgpu::TextureFormat::Rg8Unorm,
                handle.width / 2,
                handle.height / 2,
            )?);
        }
    }
    Ok(textures)
}

/// Import one plane's descriptor as a linear `VkImage`, and adopt it into a `wgpu` texture.
#[expect(clippy::too_many_arguments)]
fn import_plane(
    device: &wgpu::Device,
    hal: &wgpu::hal::vulkan::Device,
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    plane: &DmaBufPlane,
    vk_format: vk::Format,
    wgpu_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> Result<PlaneTexture> {
    let raw = hal.raw_device();
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk_format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::LINEAR)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { raw.create_image(&image_info, None) }
        .context("create the imported image")?;
    let requirements = unsafe { raw.get_image_memory_requirements(image) };

    // The descriptor is consumed by the import, so hand over a duplicate and let the driver own it.
    let fd = plane
        .fd
        .try_clone()
        .context("duplicate the plane descriptor for the import")?
        .into_raw_fd();

    let memory = import_memory(instance, physical, raw, image, requirements, fd)?;
    unsafe { raw.bind_image_memory(image, memory, plane.offset) }
        .context("bind the imported image at the plane's offset")?;

    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let hal_texture = unsafe {
        hal.texture_from_raw(
            image,
            &wgpu::hal::TextureDescriptor {
                label: Some("dma-buf surface plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu_format,
                usage: wgpu::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            },
            None,
            wgpu::hal::vulkan::TextureMemory::Dedicated(memory),
        )
    };
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::vulkan::Api>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("dma-buf surface plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
    };
    Ok(PlaneTexture { texture })
}

/// Import `fd` into a dedicated allocation for `image`, trying every memory type it can live in.
///
/// Which memory type a driver accepts a dma-buf into is not always the obvious one — the probe saw
/// a discrete GPU refuse it on every type until the allocation was dedicated — so this tries them
/// all rather than guessing.
fn import_memory(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    raw: &ash::Device,
    image: vk::Image,
    requirements: vk::MemoryRequirements,
    fd: std::os::fd::RawFd,
) -> Result<vk::DeviceMemory> {
    let properties = unsafe { instance.get_physical_device_memory_properties(physical) };
    let mut last_error = None;
    for index in 0..properties.memory_type_count {
        if requirements.memory_type_bits & (1 << index) == 0 {
            continue;
        }
        let mut import_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
            .fd(fd);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let allocate_info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(index)
            .push_next(&mut import_info)
            .push_next(&mut dedicated);
        match unsafe { raw.allocate_memory(&allocate_info, None) } {
            Ok(memory) => return Ok(memory),
            Err(error) => last_error = Some(error),
        }
    }
    Err(anyhow::anyhow!(
        "no memory type accepted the dma-buf import (last: {last_error:?})",
    ))
}
