//! Importing a dma-buf into `wgpu` textures, on the Vulkan backend.
//!
//! A [`DmaBufHandle`](gpui_engine::DmaBufHandle) is a buffer that another component — a compositor,
//! video decoder, camera or second GPU process — exports as a dma-buf: one file descriptor per plane
//! plus the layout needed to interpret it (a format, a DRM format modifier, and a byte offset and
//! row stride per plane). The renderer *consumes* such a handle to composite the buffer as a
//! surface; it does not produce or negotiate one.
//!
//! `wgpu` can adopt a raw `VkImage` (`texture_from_raw`) but cannot create one over an external
//! allocation, so the import is done with `ash` against the very device `wgpu` draws on: create the
//! image, import the descriptor into a **dedicated** allocation, bind it, and hand the image to
//! `wgpu`. The dedicated allocation is not decoration — a discrete GPU can refuse the import on
//! every memory type without it, even while advertising the format as `IMPORTABLE`, so a dedicated
//! allocation is what imports reliably across drivers.
//!
//! A descriptor is consumed by the import (the driver takes ownership), so each plane imports a
//! `dup` of its descriptor; that is also what lets an `NV12` buffer's two planes, which may share one
//! descriptor, become two independently-owned textures.
//!
//! # The producer's contract
//!
//! The importer validates the handle and refuses one it cannot honour, rather than compositing a
//! wrong image. A producer must therefore:
//!
//! - **Declare the buffer's tiling.** A single-plane-per-layer buffer is sampled as-is: the
//!   importer creates the plane's image with the handle's [`modifier`](gpui_engine::DmaBufHandle::modifier),
//!   so a producer may export a vendor-tiled buffer
//!   ([`DRM_FORMAT_MOD_LINEAR`](gpui_engine::DmaBufHandle::LINEAR) is the portable choice, and a tiled
//!   modifier is sampled only where the device enabled `VK_EXT_image_drm_format_modifier` — see
//!   `WgpuContext::device_with_surface_import` — and otherwise refused by `vkCreateImage`).
//! - **Carry one plane per format.** One plane for `Bgra8`/`Rgba8`, two for `Nv12`; a handle whose
//!   plane count does not match its format is refused.
//!
//! See [`DmaBufHandle`](gpui_engine::DmaBufHandle) for the descriptor itself and the full set of
//! invariants a dma-buf import requires.

use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

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
        .context("importing a dma-buf needs the Vulkan backend")?;
    let instance = hal.shared_instance().raw_instance();
    let physical = hal.raw_physical_device();

    let mut textures = Vec::with_capacity(expected);
    match handle.format {
        DmaBufFormat::Bgra8 | DmaBufFormat::Rgba8 => {
            let (wgpu_format, vk_format) = match handle.format {
                DmaBufFormat::Bgra8 => {
                    (wgpu::TextureFormat::Bgra8Unorm, vk::Format::B8G8R8A8_UNORM)
                }
                _ => (wgpu::TextureFormat::Rgba8Unorm, vk::Format::R8G8B8A8_UNORM),
            };
            textures.push(import_plane(
                device,
                &*hal,
                instance,
                physical,
                &handle.planes[0],
                handle.modifier,
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
                handle.modifier,
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
                handle.modifier,
                vk::Format::R8G8_UNORM,
                wgpu::TextureFormat::Rg8Unorm,
                handle.width / 2,
                handle.height / 2,
            )?);
        }
    }
    Ok(textures)
}

/// How long the renderer waits for a producer's `sync_file` before dropping the surface for that
/// frame — about one frame at 60 Hz, so a producer that is merely a frame late still composites.
///
/// A producer further behind loses a frame rather than stalling the UI, the same "fault softly"
/// rule the rest of the surface path follows.
pub(crate) const ACQUIRE_FENCE_TIMEOUT: Duration = Duration::from_millis(16);

/// Wait, up to `timeout`, for a surface's acquire fence to signal; `true` if it signalled in time.
///
/// A `sync_file` becomes readable when its fence signals, so this is a `poll(2)`. It is a *host*
/// wait on purpose: `wgpu` owns its queue and gives no hook to make one of its submissions wait on
/// an external semaphore, and a queue-level wait has no timeout — a producer that never signals
/// would stall the renderer forever. A poll can give up, so a broken producer costs a frame rather
/// than the application.
///
/// The common case is free: a producer at or ahead of the frame rate has already signalled, and
/// `poll` returns at once.
pub(crate) fn wait_for_acquire_fence(fence: &OwnedFd, timeout: Duration) -> bool {
    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut descriptor = libc::pollfd {
        fd: fence.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        match unsafe { libc::poll(&mut descriptor, 1, timeout_ms) } {
            // Readable: the fence signalled. An invalid descriptor (`POLLNVAL`) reports ready too
            // and falls through to the import, which drops the surface.
            1.. => return true,
            // Timed out: the producer is not done, so give up rather than block the frame.
            0 => return false,
            // `-1`: retry an interrupted wait (a signal), otherwise treat the fence as unusable and
            // drop the surface.
            _ => {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return false;
            }
        }
    }
}

/// Import one plane's descriptor as a `VkImage`, and adopt it into a `wgpu` texture.
///
/// `modifier` is the buffer's DRM format modifier: [`DmaBufHandle::LINEAR`](gpui_engine::DmaBufHandle::LINEAR)
/// makes a linear image, and any other value makes a tiled one whose single-`plane` layout is given
/// explicitly, which is the shape `vaExportSurfaceHandle` and GBM exports describe. A tiled image
/// needs `VK_EXT_image_drm_format_modifier` on the device, which `WgpuContext` enables; a device
/// without it refuses the image, and the surface is dropped rather than sampled wrong.
#[expect(clippy::too_many_arguments)]
fn import_plane(
    device: &wgpu::Device,
    hal: &wgpu::hal::vulkan::Device,
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    plane: &DmaBufPlane,
    modifier: u64,
    vk_format: vk::Format,
    wgpu_format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> Result<PlaneTexture> {
    let raw = hal.raw_device();
    let tiled = modifier != DmaBufHandle::LINEAR;
    // The image binds at the plane's offset within the object, so the plane's own layout starts at
    // zero; its row pitch is the buffer's. The driver validates both against the modifier.
    let plane_layout = vk::SubresourceLayout::default()
        .offset(0)
        .row_pitch(u64::from(plane.stride));
    let mut modifier_layout = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(modifier)
        .plane_layouts(std::slice::from_ref(&plane_layout));
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
        .tiling(if tiled {
            vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
        } else {
            vk::ImageTiling::LINEAR
        })
        .usage(vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image_info = if tiled {
        image_info.push_next(&mut modifier_layout)
    } else {
        image_info
    };
    let image =
        unsafe { raw.create_image(&image_info, None) }.context("create the imported image")?;
    let requirements = unsafe { raw.get_image_memory_requirements(image) };

    // The descriptor is consumed by the import, so hand over a duplicate and let the driver own it.
    let fd = plane
        .fd
        .try_clone()
        .context("duplicate the plane descriptor for the import")?
        .into_raw_fd();

    let memory = import_memory(
        instance,
        physical,
        raw,
        image,
        requirements,
        plane.offset,
        fd,
    )?;
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
/// `offset` is where the image binds within the buffer, so the allocation must span
/// `offset + requirements.size` — a plane at a non-zero offset (an `NV12` buffer's chroma) needs the
/// whole buffer, not just its own tail. Which memory type a driver accepts a dma-buf into is not
/// always the obvious one — a discrete GPU can refuse the import on every type until the allocation
/// is dedicated, and the one type that imports may not be the one advertising the format as
/// `IMPORTABLE` — so this tries them all rather than guessing.
fn import_memory(
    instance: &ash::Instance,
    physical: vk::PhysicalDevice,
    raw: &ash::Device,
    image: vk::Image,
    requirements: vk::MemoryRequirements,
    offset: u64,
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
            .allocation_size(offset + requirements.size)
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

/// Reuses the textures imported for a dma-buf across frames.
///
/// The Apple backend has the platform's `CVMetalTextureCache` for exactly this: a CoreVideo texture cache
/// held on the renderer that hands back the same `MTLTexture` for the same `CVPixelBuffer`. There is
/// no such facility for dma-bufs, so this is it — and it is what keeps painting a live surface from
/// re-creating a `VkImage`, a dedicated allocation and a `wgpu::Texture` every frame.
///
/// Identity is the handle's *descriptors*, compared by `Arc` identity, never the numbers. A producer
/// that keeps its handle (the natural way — `surface(handle.clone())` each frame) hits, and a
/// different buffer never aliases, even if the kernel hands the same descriptor number back.
pub(crate) struct DmaBufTextureCache {
    entries: Vec<CacheEntry>,
    frame: u64,
}

struct CacheEntry {
    handle: DmaBufHandle,
    textures: Vec<PlaneTexture>,
    last_used: u64,
}

/// At most this many buffers stay imported. A producer streams from a small pool — two or three
/// buffers — so this covers the recycling without pinning an unbounded number of its buffers.
const CAPACITY: usize = 8;

impl DmaBufTextureCache {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            frame: 0,
        }
    }

    /// The textures for `handle`, importing them the first time this buffer is seen since it was
    /// last evicted.
    ///
    /// Clones are returned (cheap handles into the same allocation); the cache keeps the originals,
    /// and with them the imported memory.
    pub(crate) fn textures(
        &mut self,
        device: &wgpu::Device,
        handle: &DmaBufHandle,
    ) -> Result<Vec<wgpu::Texture>> {
        self.frame += 1;
        let frame = self.frame;

        if let Some(index) = self.find(handle) {
            self.entries[index].last_used = frame;
            return Ok(self.entries[index]
                .textures
                .iter()
                .map(|plane| plane.texture.clone())
                .collect());
        }

        let imported = import_dmabuf(device, handle)?;
        let textures = imported.iter().map(|plane| plane.texture.clone()).collect();

        self.evict_if_full();
        self.entries.push(CacheEntry {
            handle: handle.clone(),
            textures: imported,
            last_used: frame,
        });
        Ok(textures)
    }

    /// The index of the entry for `handle`, if this buffer is already imported.
    fn find(&self, handle: &DmaBufHandle) -> Option<usize> {
        self.entries
            .iter()
            .position(|entry| same_buffer(&entry.handle, handle))
    }

    /// Make room for one more entry, dropping the least recently used when full, so a recycled
    /// buffer is re-imported rather than pinned forever.
    fn evict_if_full(&mut self) {
        if self.entries.len() < CAPACITY {
            return;
        }
        if let Some((index, _)) = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.last_used)
        {
            self.entries.swap_remove(index);
        }
    }
}

/// Whether two handles describe the same buffer: the same descriptors (by `Arc` identity), layout
/// and format. Deliberately ignores the acquire fence, which is per frame, not per buffer.
fn same_buffer(a: &DmaBufHandle, b: &DmaBufHandle) -> bool {
    a.width == b.width
        && a.height == b.height
        && a.format == b.format
        && a.modifier == b.modifier
        && a.planes.len() == b.planes.len()
        && a.planes
            .iter()
            .zip(&b.planes)
            .all(|(a, b)| Arc::ptr_eq(&a.fd, &b.fd) && a.offset == b.offset && a.stride == b.stride)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;

    /// A real descriptor without a GPU: `/dev/null` is an open file, and any open file is a valid
    /// plane descriptor as far as the descriptor type is concerned.
    fn descriptor() -> OwnedFd {
        std::fs::File::open("/dev/null").expect("/dev/null").into()
    }

    fn handle(width: u32, offset: u64, stride: u32) -> DmaBufHandle {
        DmaBufHandle::new(
            width,
            2,
            DmaBufFormat::Bgra8,
            DmaBufHandle::LINEAR,
            [DmaBufPlane::new(descriptor(), offset, stride)],
            None,
        )
    }

    #[test]
    fn a_cloned_handle_is_the_same_buffer() {
        // The natural producer pattern: keep the handle, hand the renderer a clone each frame.
        let handle = handle(4, 0, 16);
        assert!(same_buffer(&handle, &handle.clone()));
    }

    #[test]
    fn layout_and_format_differences_are_different_buffers() {
        let base = handle(4, 0, 16);
        assert!(!same_buffer(&base, &handle(8, 0, 16)), "width");
        assert!(!same_buffer(&base, &handle(4, 16, 16)), "offset");
        assert!(!same_buffer(&base, &handle(4, 0, 32)), "stride");

        let rgba = DmaBufHandle::new(
            4,
            2,
            DmaBufFormat::Rgba8,
            DmaBufHandle::LINEAR,
            [DmaBufPlane::new(descriptor(), 0, 16)],
            None,
        );
        assert!(!same_buffer(&base, &rgba), "format");
    }

    #[test]
    fn a_different_descriptor_is_a_different_buffer() {
        // Two handles built from separate `/dev/null` opens: different descriptors, so no aliasing
        // even though they describe the same shape.
        assert!(!same_buffer(&handle(4, 0, 16), &handle(4, 0, 16)));
    }

    #[test]
    fn the_fence_does_not_affect_identity() {
        // The fence is per frame, not per buffer: a producer that attaches a fresh sync_file each
        // frame must still hit the cache.
        let handle = handle(4, 0, 16);
        let mut with_fence = handle.clone();
        with_fence.acquire_fence = Some(Arc::new(descriptor()));
        assert!(same_buffer(&handle, &with_fence));
    }

    fn entry(handle: DmaBufHandle, last_used: u64) -> CacheEntry {
        CacheEntry {
            handle,
            textures: Vec::new(),
            last_used,
        }
    }

    #[test]
    fn find_locates_the_matching_buffer() {
        let buffer = handle(4, 0, 16);
        let other = handle(8, 0, 16);
        let mut cache = DmaBufTextureCache::new();
        assert_eq!(cache.find(&buffer), None);
        cache.entries.push(entry(buffer.clone(), 1));
        cache.entries.push(entry(other.clone(), 1));
        assert_eq!(cache.find(&buffer), Some(0));
        assert_eq!(cache.find(&other), Some(1));
    }

    #[test]
    fn a_full_cache_evicts_the_least_recently_used() {
        let buffers: Vec<_> = (0..CAPACITY)
            .map(|i| handle((i + 1) as u32, 0, 16))
            .collect();
        let mut cache = DmaBufTextureCache::new();
        for (i, buffer) in buffers.iter().enumerate() {
            cache.entries.push(entry(buffer.clone(), i as u64));
        }
        // Touch everything but the oldest.
        for entry in &mut cache.entries[1..] {
            entry.last_used = 100;
        }
        cache.evict_if_full();
        assert_eq!(cache.entries.len(), CAPACITY - 1);
        assert!(
            cache.find(&buffers[0]).is_none(),
            "the oldest entry was evicted"
        );
        for buffer in &buffers[1..] {
            assert!(cache.find(buffer).is_some());
        }
    }

    #[test]
    fn a_cache_with_room_evicts_nothing() {
        let mut cache = DmaBufTextureCache::new();
        cache.entries.push(entry(handle(4, 0, 16), 1));
        cache.evict_if_full();
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn an_already_signalled_fence_does_not_block() {
        // `/dev/null` is always readable — the shape of a `sync_file` whose fence has signalled.
        let fence: OwnedFd = std::fs::File::open("/dev/null").expect("/dev/null").into();
        assert!(wait_for_acquire_fence(&fence, Duration::from_millis(100)));
    }

    #[test]
    fn a_fence_that_never_signals_times_out() {
        // A pipe with no data is the shape of a producer that has not finished; the wait must give
        // up rather than hang. The write end is held open (a dropped writer reads as EOF, which is
        // `ready`), so nothing ever signals.
        let (read_end, _write_end) = std::io::pipe().expect("a pipe");
        let fence = OwnedFd::from(read_end);
        let started = std::time::Instant::now();
        assert!(!wait_for_acquire_fence(&fence, Duration::from_millis(10)));
        assert!(
            started.elapsed() >= Duration::from_millis(5),
            "it should have waited"
        );
    }
}
