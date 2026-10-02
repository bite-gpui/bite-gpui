//! Probe P3: can a non-wgpu producer hand GPUI's Linux (wgpu/Vulkan) renderer a dma-buf?
//!
//! `0002` dismissed the Linux bridge as "unnecessary: on Linux both sides are wgpu" — true of the
//! same-device case and only that. A cross-process or cross-API producer (a VA-API/NVDEC decoder, a
//! Wayland client, an engine on its own device) needs the dma-buf transport, and nothing has been
//! measured on Linux at all. This prints what each step can actually do rather than assuming an
//! answer, in order:
//!
//! 1. the environment: which adapters wgpu sees, and which raw Vulkan devices exist with which
//!    external-memory extensions (the gate — no adapter with the extensions is a result, not a bug);
//! 2. the producer: allocate a dma-buf outside wgpu, export its fd, and write known bytes into it;
//! 3. the consumer: import that fd, and read the bytes back unchanged.
//!
//! A negative answer at any step is a result. The printout is the evidence, recorded in
//! `decisions/` beside the surface decision. Run on Linux:
//! `cargo run --manifest-path probes/linux-dmabuf/Cargo.toml`.

use std::collections::HashSet;
use std::os::raw::c_char;

use anyhow::{Context as _, Result};

/// The device extensions an external-memory import needs. Four of them, because an import is the
/// memory (external fd, dma-buf), the layout (format list), and the bind (bind memory 2).
const NEEDED_EXTENSIONS: [&str; 4] = [
    "VK_KHR_external_memory_fd",
    "VK_EXT_external_memory_dma_buf",
    "VK_KHR_image_format_list",
    "VK_KHR_bind_memory2",
];

/// The size of the probe's buffer: 16x16 RGBA pixels, tightly packed.
const PIXELS: u32 = 16;

/// A format a producer emits: the Vulkan format, the wgpu format it maps to, the fourcc name, and
/// the bytes of a solid colour (R=32, G=192, B=64, A=255) in that format's memory layout.
struct Format {
    vk_format: ash::vk::Format,
    wgpu_format: wgpu::TextureFormat,
    fourcc: &'static str,
    /// The byte order in memory for the solid colour: `[R, G, B, A]` for `R8G8B8A8`, `[B, G, R, A]`
    /// for `B8G8R8A8`. The sampled channels always read back as R=32, G=192, B=64.
    solid: [u8; 4],
}

const FORMATS: [Format; 2] = [
    Format {
        vk_format: ash::vk::Format::R8G8B8A8_UNORM,
        wgpu_format: wgpu::TextureFormat::Rgba8Unorm,
        fourcc: "ABGR8888",
        solid: [32, 192, 64, 255],
    },
    Format {
        vk_format: ash::vk::Format::B8G8R8A8_UNORM,
        wgpu_format: wgpu::TextureFormat::Bgra8Unorm,
        fourcc: "ARGB8888",
        solid: [64, 192, 32, 255],
    },
];

fn main() -> Result<()> {
    print_wgpu_adapters();
    let entry =
        unsafe { ash::Entry::load().map_err(|error| anyhow::anyhow!("load vulkan: {error}"))? };
    let (instance, devices) = vulkan_devices(&entry)?;

    print_vulkan_devices(&instance, &devices)?;

    // Stage 2+3 — pick a hardware device (prefer integrated, which is what wgpu picks by default
    // on a hybrid laptop in power-saving mode) and run the dma-buf round trip on it.
    let chosen = devices
        .iter()
        .find(|&&device| {
            let properties = unsafe { instance.get_physical_device_properties(device) };
            properties.device_type == ash::vk::PhysicalDeviceType::INTEGRATED_GPU
        })
        .or(devices.first());
    match chosen {
        Some(&device) => {
            for format in &FORMATS {
                run_dmabuf_round_trip(&instance, device, format)?;
            }
            // A vendor-tiled modifier with a single plane: the import the flat-linear pass does not
            // cover.
            run_tiled_round_trip(&instance, device, 0x0100_0000_0000_0002)?;
            // The `sync_file` fence: the Linux counterpart of the keyed mutex and `MTLSharedEvent`.
            run_fence_probe(&instance, device)?;
        }
        None => println!("no physical device to run the round trip on"),
    }
    Ok(())
}

/// Stage 1a — the adapters the renderer itself would see. The Linux renderer is wgpu on Vulkan, so
/// this is the consumer's own view of the machine.
fn print_wgpu_adapters() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        flags: wgpu::InstanceFlags::default(),
        backend_options: wgpu::BackendOptions::default(),
        memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        display: None,
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    println!("=== wgpu adapters: {} ===", adapters.len());
    for adapter in &adapters {
        println!("  {:?}", adapter.get_info());
    }
    if adapters.is_empty() {
        println!("  (none)");
    }
}

/// Creates a bare Vulkan instance and returns it with its physical devices.
fn vulkan_devices(entry: &ash::Entry) -> Result<(ash::Instance, Vec<ash::vk::PhysicalDevice>)> {
    let app_info =
        ash::vk::ApplicationInfo::default().api_version(ash::vk::make_api_version(0, 1, 3, 0));
    let create_info = ash::vk::InstanceCreateInfo::default().application_info(&app_info);
    let instance = unsafe {
        entry
            .create_instance(&create_info, None)
            .context("create instance")?
    };
    let devices = unsafe {
        instance
            .enumerate_physical_devices()
            .context("enumerate devices")?
    };
    Ok((instance, devices))
}

/// Stage 1b — the raw Vulkan devices behind wgpu, and which of the import extensions each enables.
/// wgpu hides the raw extension list, so this is what answers the gate question directly.
fn print_vulkan_devices(
    instance: &ash::Instance,
    devices: &[ash::vk::PhysicalDevice],
) -> Result<()> {
    println!("=== vulkan physical devices: {} ===", devices.len());
    for &device in devices {
        let properties = unsafe { instance.get_physical_device_properties(device) };
        let name = c_char_slice_to_string(&properties.device_name);
        let api = properties.api_version;
        println!(
            "  device: name={name} type={:?} api={}.{}.{} vendor={:#06x} id={:#06x}",
            properties.device_type,
            ash::vk::api_version_major(api),
            ash::vk::api_version_minor(api),
            ash::vk::api_version_patch(api),
            properties.vendor_id,
            properties.device_id,
        );

        let extension_properties = unsafe {
            instance
                .enumerate_device_extension_properties(device)
                .context("enumerate extensions")?
        };
        let names: HashSet<String> = extension_properties
            .iter()
            .map(|extension| c_char_slice_to_string(&extension.extension_name))
            .collect();

        for needed in NEEDED_EXTENSIONS {
            let present = names.contains(needed);
            println!(
                "      {needed}: {}",
                if present { "present" } else { "absent" }
            );
        }
    }
    if devices.is_empty() {
        println!("  (none)");
    }
    Ok(())
}

/// Stage 4 — the tiled (vendor) modifiers the driver offers for a sampled image, via
/// `VK_EXT_image_drm_format_modifier`. The list is queried with `vkGetPhysicalDeviceFormatProperties2`
/// (not the image query): `VkDrmFormatModifierPropertiesListEXT` extends `VkFormatProperties2`. A real
/// producer emits a tiled layout with a vendor modifier, not only `DRM_FORMAT_MOD_LINEAR`; if none is
/// listed, the flat-linear pass is the whole story.
fn print_modifiers(instance: &ash::Instance, physical_device: ash::vk::PhysicalDevice) {
    use ash::vk;

    // Pass 1: how many modifiers does the driver report?
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    let mut format_properties = vk::FormatProperties2::default().push_next(&mut list);
    unsafe {
        instance.get_physical_device_format_properties2(
            physical_device,
            vk::Format::R8G8B8A8_UNORM,
            &mut format_properties,
        );
    }
    let count = list.drm_format_modifier_count;

    if count == 0 {
        println!("modifiers: none reported for R8G8B8A8_UNORM");
        return;
    }

    // Pass 2: the modifiers themselves.
    let mut modifiers = vec![vk::DrmFormatModifierPropertiesEXT::default(); count as usize];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default()
        .drm_format_modifier_properties(&mut modifiers);
    let mut format_properties = vk::FormatProperties2::default().push_next(&mut list);
    unsafe {
        instance.get_physical_device_format_properties2(
            physical_device,
            vk::Format::R8G8B8A8_UNORM,
            &mut format_properties,
        );
    }

    println!("modifiers for R8G8B8A8_UNORM:");
    for modifier in &modifiers {
        let sampled = modifier
            .drm_format_modifier_tiling_features
            .contains(vk::FormatFeatureFlags::SAMPLED_IMAGE);
        println!(
            "  {} ({:#018x}), planes {}, sampled {sampled}",
            modifier_name(modifier.drm_format_modifier),
            modifier.drm_format_modifier,
            modifier.drm_format_modifier_plane_count,
        );
    }
}

/// The name of a well-known DRM format modifier, or a placeholder for a vendor one (the caller prints
/// the hex value beside it).
fn modifier_name(modifier: u64) -> &'static str {
    match modifier {
        0 => "DRM_FORMAT_MOD_LINEAR",
        0x0100_0000_0000_0001 => "I915_FORMAT_MOD_X_TILED",
        0x0100_0000_0000_0002 => "I915_FORMAT_MOD_Y_TILED",
        0x0100_0000_0000_0003 => "I915_FORMAT_MOD_Yf_TILED",
        0x0100_0000_0000_0004 => "I915_FORMAT_MOD_Y_TILED_CCS",
        0x0100_0000_0000_0009 => "I915_FORMAT_MOD_4_TILED",
        _ => "vendor",
    }
}

/// Stage 6 — the `sync_file` fence: the Linux counterpart of the keyed mutex and the
/// `MTLSharedEvent`. The producer clears the image and signals a semaphore exported as a
/// `SYNC_FD`; the consumer imports that fd and waits on it before copying the image out — a GPU-side
/// order with no CPU stall between the two submissions.
fn run_fence_probe(
    instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
) -> Result<()> {
    use ash::khr::external_semaphore_fd::Device as ExternalSemaphoreFd;
    use ash::vk;

    println!("=== sync_file fence probe ===");

    let queue_families =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let queue_family_index = queue_families
        .iter()
        .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .context("no graphics queue family")? as u32;

    let extension_names = [
        vk::KHR_EXTERNAL_MEMORY_FD_NAME.as_ptr(),
        vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME.as_ptr(),
        vk::KHR_EXTERNAL_SEMAPHORE_FD_NAME.as_ptr(),
        vk::KHR_BIND_MEMORY2_NAME.as_ptr(),
    ];
    let queue_priorities = [1.0f32];
    let queue_create_info = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&queue_priorities);
    let queue_create_infos = [queue_create_info];
    let device_create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_create_infos)
        .enabled_extension_names(&extension_names);
    let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
        .context("create fence device")?;
    let external_semaphore_fd = ExternalSemaphoreFd::new(instance, &device);
    let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
    let command_pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family_index)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )
    }
    .context("fence command pool")?;
    let command_buffers = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(2),
        )
    }
    .context("fence command buffers")?;
    let (producer_command_buffer, consumer_command_buffer) =
        (command_buffers[0], command_buffers[1]);

    // A linear image the producer clears and the consumer copies out.
    let image_create_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: PIXELS,
            height: PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::LINEAR)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { device.create_image(&image_create_info, None) }.context("fence image")?;
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let memory_properties =
        unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let memory_type_index = memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            requirements.memory_type_bits & (1 << index) != 0
                && memory_type
                    .property_flags
                    .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .map(|(index, _)| index as u32)
        .context("no device-local memory for the fence image")?;
    let mut export_info = vk::ExportMemoryAllocateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index)
        .push_next(&mut export_info);
    let memory =
        unsafe { device.allocate_memory(&allocate_info, None) }.context("fence image memory")?;
    unsafe { device.bind_image_memory(image, memory, 0) }.context("bind fence image")?;

    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);

    // --- producer: clear, signal a SYNC_FD semaphore, export it ----------
    let producer_semaphore = unsafe {
        device.create_semaphore(
            &vk::SemaphoreCreateInfo::default().push_next(
                &mut vk::ExportSemaphoreCreateInfo::default()
                    .handle_types(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD),
            ),
            None,
        )
    }
    .context("producer semaphore")?;
    let clear_color = vk::ClearColorValue {
        float32: [32.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0, 1.0],
    };
    unsafe {
        device
            .begin_command_buffer(
                producer_command_buffer,
                &vk::CommandBufferBeginInfo::default(),
            )
            .context("begin producer")?;
        let barrier = vk::ImageMemoryBarrier::default()
            .image(image)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(subresource_range);
        device.cmd_pipeline_barrier(
            producer_command_buffer,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
        device.cmd_clear_color_image(
            producer_command_buffer,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &clear_color,
            &[subresource_range],
        );
        device
            .end_command_buffer(producer_command_buffer)
            .context("end producer")?;
    }
    let producer_command_buffers = [producer_command_buffer];
    let signal_semaphores = [producer_semaphore];
    let submit_info = vk::SubmitInfo::default()
        .command_buffers(&producer_command_buffers)
        .signal_semaphores(&signal_semaphores);
    unsafe { device.queue_submit(queue, &[submit_info], vk::Fence::null()) }
        .context("submit producer")?;

    // Export the fence the producer just signaled, as a `sync_file` fd. No `queue_wait_idle`: the fd
    // is the pending GPU fence, which is the point.
    let get_fd_info = vk::SemaphoreGetFdInfoKHR::default()
        .semaphore(producer_semaphore)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD);
    let fence_fd = unsafe { external_semaphore_fd.get_semaphore_fd(&get_fd_info) }
        .context("export fence as a sync_file")?;
    println!("producer: cleared, signaled, exported sync_file fd {fence_fd}");

    // --- consumer: import the fd, wait on it, copy the image out ---------
    let consumer_semaphore =
        unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
            .context("consumer semaphore")?;
    let import_info = vk::ImportSemaphoreFdInfoKHR::default()
        .semaphore(consumer_semaphore)
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::SYNC_FD)
        .fd(fence_fd);
    unsafe { external_semaphore_fd.import_semaphore_fd(&import_info) }
        .context("import the sync_file")?;

    let readback_size = (PIXELS * PIXELS * 4) as u64;
    let readback_buffer = unsafe {
        device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(readback_size)
                .usage(vk::BufferUsageFlags::TRANSFER_DST),
            None,
        )
    }
    .context("fence readback buffer")?;
    let readback_requirements = unsafe { device.get_buffer_memory_requirements(readback_buffer) };
    let readback_memory_type_index = memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            readback_requirements.memory_type_bits & (1 << index) != 0
                && memory_type.property_flags.contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
        })
        .map(|(index, _)| index as u32)
        .context("no host-visible memory for the fence readback")?;
    let readback_memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(readback_requirements.size)
                .memory_type_index(readback_memory_type_index),
            None,
        )
    }
    .context("fence readback memory")?;
    unsafe { device.bind_buffer_memory(readback_buffer, readback_memory, 0) }
        .context("bind fence readback")?;

    unsafe {
        device
            .begin_command_buffer(
                consumer_command_buffer,
                &vk::CommandBufferBeginInfo::default(),
            )
            .context("begin consumer")?;
        let barrier = vk::ImageMemoryBarrier::default()
            .image(image)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .subresource_range(subresource_range);
        device.cmd_pipeline_barrier(
            consumer_command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
        let region = vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .mip_level(0)
                    .base_array_layer(0)
                    .layer_count(1),
            )
            .image_offset(vk::Offset3D::default())
            .image_extent(vk::Extent3D {
                width: PIXELS,
                height: PIXELS,
                depth: 1,
            });
        device.cmd_copy_image_to_buffer(
            consumer_command_buffer,
            image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            readback_buffer,
            &[region],
        );
        device
            .end_command_buffer(consumer_command_buffer)
            .context("end consumer")?;
    }
    let consumer_command_buffers = [consumer_command_buffer];
    let wait_semaphores = [consumer_semaphore];
    let wait_stages = [vk::PipelineStageFlags::TRANSFER];
    let submit_info = vk::SubmitInfo::default()
        .command_buffers(&consumer_command_buffers)
        .wait_semaphores(&wait_semaphores)
        .wait_dst_stage_mask(&wait_stages);
    unsafe { device.queue_submit(queue, &[submit_info], vk::Fence::null()) }
        .context("submit consumer")?;
    unsafe { device.queue_wait_idle(queue) }.context("wait consumer")?;

    let mapped = unsafe {
        device.map_memory(
            readback_memory,
            0,
            readback_size,
            vk::MemoryMapFlags::empty(),
        )
    }
    .context("map fence readback")? as *const u8;
    let bytes = unsafe { std::slice::from_raw_parts(mapped, readback_size as usize) };
    let expected = [32u8, 192, 64, 255];
    let matches = bytes.chunks(4).all(|pixel| pixel == expected);
    println!(
        "fence: producer's clear read back after a sync_file wait — {}",
        if matches {
            "MATCH, no tear"
        } else {
            "MISMATCH"
        }
    );
    unsafe { device.unmap_memory(readback_memory) };

    unsafe {
        device.destroy_semaphore(producer_semaphore, None);
        device.destroy_semaphore(consumer_semaphore, None);
        device.destroy_image(image, None);
        device.destroy_buffer(readback_buffer, None);
        device.free_memory(memory, None);
        device.free_memory(readback_memory, None);
        device.destroy_command_pool(command_pool, None);
        device.destroy_device(None);
    }
    Ok(())
}

/// Stage 5 — a tiled (vendor-modifier) round trip: the producer creates a tiled image, fills it from
/// a linear staging buffer, exports its fd, modifier and plane layout, and the consumer imports the
/// fd, pins its image to that exact layout with `VkImageDrmFormatModifierExplicitCreateInfoEXT`, and
/// reads the bytes back. The fill is a gradient (not a clear): a tiled image cannot be mapped, and a
/// solid colour reads back solid under every tiling, so a clear would hide a layout mismatch.
///
/// The producer requests `STORAGE` usage to stop ANV enabling implicit (CCS) compression: that
/// compression state is not carried by the single-plane `I915_FORMAT_MOD_Y_TILED` dma-buf, so a
/// compressed producer imports as garbage. With an uncompressed surface the round trip is byte-exact
/// — a tiled dma-buf is importable and sampleable with no CPU copy.
fn run_tiled_round_trip(
    instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
    modifier: u64,
) -> Result<()> {
    use ash::khr::external_memory_fd::Device as ExternalMemoryFd;
    use ash::vk;

    println!(
        "=== tiled round trip, {} ({modifier:#018x}) ===",
        modifier_name(modifier)
    );

    // Does the driver advertise compression control (and thereby expose whether it compresses
    // tiled images by default)?
    {
        let extensions = unsafe { instance.enumerate_device_extension_properties(physical_device) }
            .unwrap_or_default();
        let names: Vec<String> = extensions
            .iter()
            .map(|e| c_char_slice_to_string(&e.extension_name))
            .collect();
        for name in [
            "VK_EXT_image_compression_control",
            "VK_EXT_image_drm_format_modifier",
        ] {
            println!("  {name}: {}", names.iter().any(|n| n == name));
        }
    }

    // Does the driver even allow dma-buf export/import for this modifier? Query the external
    // capabilities of the tiled format before doing anything else.
    {
        let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut drm_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(modifier)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let image_info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(vk::Format::R8G8B8A8_UNORM)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC
                    | vk::ImageUsageFlags::TRANSFER_DST,
            )
            .push_next(&mut external_info)
            .push_next(&mut drm_info);
        let mut external_props = vk::ExternalImageFormatProperties::default();
        let mut image_props = vk::ImageFormatProperties2::default().push_next(&mut external_props);
        match unsafe {
            instance.get_physical_device_image_format_properties2(
                physical_device,
                &image_info,
                &mut image_props,
            )
        } {
            Ok(()) => println!(
                "  external dma-buf features: {:?}, compatible handle types: {:?}",
                external_props
                    .external_memory_properties
                    .external_memory_features,
                external_props
                    .external_memory_properties
                    .compatible_handle_types,
            ),
            Err(code) => println!("  external dma-buf query failed: {code:?}"),
        }
    }

    let queue_families =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let queue_family_index = queue_families
        .iter()
        .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .context("no graphics queue family")? as u32;

    let extension_names = [
        vk::KHR_EXTERNAL_MEMORY_FD_NAME.as_ptr(),
        vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME.as_ptr(),
        vk::KHR_IMAGE_FORMAT_LIST_NAME.as_ptr(),
        vk::KHR_BIND_MEMORY2_NAME.as_ptr(),
        vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME.as_ptr(),
    ];
    let queue_priorities = [1.0f32];
    let queue_create_info = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&queue_priorities);
    let queue_create_infos = [queue_create_info];
    let device_create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_create_infos)
        .enabled_extension_names(&extension_names);
    let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
        .context("create device")?;
    let external_memory_fd = ExternalMemoryFd::new(instance, &device);
    let ext_drm = ash::ext::image_drm_format_modifier::Device::new(instance, &device);
    let queue = unsafe { device.get_device_queue(queue_family_index, 0) };
    let command_pool = unsafe {
        device.create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(queue_family_index)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )
    }
    .context("command pool")?;
    let command_buffer = unsafe {
        device.allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(command_pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(1),
        )
    }
    .context("command buffer")?[0];

    let memory_properties =
        unsafe { instance.get_physical_device_memory_properties(physical_device) };

    // The producer's source pattern: a gradient, so a mis-imported tiling shows up as scrambled
    // bytes rather than a solid colour that every layout would reproduce.
    let mut pattern = vec![0u8; (PIXELS * PIXELS * 4) as usize];
    for y in 0..PIXELS {
        for x in 0..PIXELS {
            let offset = ((y * PIXELS + x) * 4) as usize;
            pattern[offset] = (x * 16) as u8;
            pattern[offset + 1] = (y * 16) as u8;
            pattern[offset + 2] = 128;
            pattern[offset + 3] = 255;
        }
    }

    // --- producer: a tiled image, filled from a linear staging buffer, then exported ---
    let modifiers = [modifier];
    let mut modifier_list =
        vk::ImageDrmFormatModifierListCreateInfoEXT::default().drm_format_modifiers(&modifiers);
    let mut producer_external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let image_create_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: PIXELS,
            height: PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        // STORAGE is load-bearing, not cosmetic: ANV enables lossless (CCS) compression by default
        // for a sampled Y_TILED R8G8B8A8 image, and that compression state is *not* carried by the
        // exported single-plane `I915_FORMAT_MOD_Y_TILED` dma-buf. A consumer importing the fd then
        // reads the compressed payload as raw bytes and the image is garbage. Requesting STORAGE
        // makes ANV allocate the surface uncompressed, so the plane is self-describing and the
        // import is byte-exact.
        .usage(
            vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::STORAGE,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut modifier_list)
        .push_next(&mut producer_external);
    let producer_image = unsafe { device.create_image(&image_create_info, None) }
        .context("create tiled producer image")?;
    let requirements = unsafe { device.get_image_memory_requirements(producer_image) };

    // DIAGNOSTIC: host-visible so the shared memory can be poked from the CPU.
    let memory_type_index = memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            requirements.memory_type_bits & (1 << index) != 0
                && memory_type.property_flags.contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
        })
        .map(|(index, _)| index as u32)
        .context("no host-visible image memory type")?;

    let mut export_info = vk::ExportMemoryAllocateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut producer_dedicated = vk::MemoryDedicatedAllocateInfo::default().image(producer_image);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type_index)
        .push_next(&mut export_info)
        .push_next(&mut producer_dedicated);
    let memory = unsafe { device.allocate_memory(&allocate_info, None) }
        .context("allocate tiled export memory")?;
    unsafe { device.bind_image_memory(producer_image, memory, 0) }
        .context("bind tiled producer image")?;

    // A linear, host-visible staging buffer holding the source pattern.
    let pattern_size = (PIXELS * PIXELS * 4) as u64;
    let staging_buffer = unsafe {
        device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(pattern_size)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC),
            None,
        )
    }
    .context("staging buffer")?;
    let staging_requirements = unsafe { device.get_buffer_memory_requirements(staging_buffer) };
    let staging_memory_type_index = memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            staging_requirements.memory_type_bits & (1 << index) != 0
                && memory_type.property_flags.contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
        })
        .map(|(index, _)| index as u32)
        .context("no host-visible memory for staging")?;
    let staging_memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(staging_requirements.size)
                .memory_type_index(staging_memory_type_index),
            None,
        )
    }
    .context("staging memory")?;
    unsafe { device.bind_buffer_memory(staging_buffer, staging_memory, 0) }
        .context("bind staging")?;
    let staging_mapped =
        unsafe { device.map_memory(staging_memory, 0, pattern_size, vk::MemoryMapFlags::empty()) }
            .context("map staging")? as *mut u8;
    unsafe { std::ptr::copy_nonoverlapping(pattern.as_ptr(), staging_mapped, pattern.len()) };
    unsafe { device.unmap_memory(staging_memory) };

    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);
    let region = vk::BufferImageCopy::default()
        .buffer_offset(0)
        .buffer_row_length(0)
        .buffer_image_height(0)
        .image_subresource(
            vk::ImageSubresourceLayers::default()
                .aspect_mask(vk::ImageAspectFlags::COLOR)
                .mip_level(0)
                .base_array_layer(0)
                .layer_count(1),
        )
        .image_offset(vk::Offset3D::default())
        .image_extent(vk::Extent3D {
            width: PIXELS,
            height: PIXELS,
            depth: 1,
        });
    unsafe {
        device
            .begin_command_buffer(command_buffer, &vk::CommandBufferBeginInfo::default())
            .context("begin command buffer")?;
        let to_dst = vk::ImageMemoryBarrier::default()
            .image(producer_image)
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .subresource_range(subresource_range);
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_dst],
        );
        device.cmd_copy_buffer_to_image(
            command_buffer,
            staging_buffer,
            producer_image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
        // Hand the buffer off in GENERAL: the layout a cross-process consumer is told to expect.
        let to_general = vk::ImageMemoryBarrier::default()
            .image(producer_image)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::GENERAL)
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
            .subresource_range(subresource_range);
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_general],
        );
        device
            .end_command_buffer(command_buffer)
            .context("end command buffer")?;
    }
    let command_buffers = [command_buffer];
    let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers);
    unsafe { device.queue_submit(queue, &[submit_info], vk::Fence::null()) }
        .context("submit fill")?;
    unsafe { device.queue_wait_idle(queue) }.context("wait fill")?;

    // Read back the modifier the driver chose and the producer's exact plane layout. For a DRM
    // image the layout is queried with the memory-plane aspect, not COLOR.
    let mut chosen_modifier = vk::ImageDrmFormatModifierPropertiesEXT::default();
    unsafe {
        ext_drm.get_image_drm_format_modifier_properties(producer_image, &mut chosen_modifier)
    }
    .context("producer modifier")?;
    let producer_plane = unsafe {
        device.get_image_subresource_layout(
            producer_image,
            vk::ImageSubresource::default()
                .aspect_mask(vk::ImageAspectFlags::MEMORY_PLANE_0_EXT)
                .mip_level(0)
                .array_layer(0),
        )
    };
    println!(
        "producer: modifier {:#x}, plane layout offset={} size={} row_pitch={}",
        chosen_modifier.drm_format_modifier,
        producer_plane.offset,
        producer_plane.size,
        producer_plane.row_pitch,
    );

    let get_fd_info = vk::MemoryGetFdInfoKHR::default()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let fd =
        unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }.context("export tiled fd")?;
    println!("producer: exported fd {fd}, {PIXELS}x{PIXELS} tiled");

    // --- consumer: create the image pinned to the producer's modifier and plane layout, then import
    // the fd as a dedicated allocation (a DRM-modifier image requires one) ---
    let plane_layouts = [producer_plane];
    let mut explicit_create = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(chosen_modifier.drm_format_modifier)
        .plane_layouts(&plane_layouts);
    let mut consumer_external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let consumer_image_create_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: PIXELS,
            height: PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(
            vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::SAMPLED,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut explicit_create)
        .push_next(&mut consumer_external);
    let consumer_image = unsafe { device.create_image(&consumer_image_create_info, None) }
        .context("create explicit-layout consumer image")?;
    let consumer_requirements = unsafe { device.get_image_memory_requirements(consumer_image) };

    let mut import_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(fd);
    let mut consumer_dedicated = vk::MemoryDedicatedAllocateInfo::default().image(consumer_image);
    let import_allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(consumer_requirements.size)
        .memory_type_index(memory_type_index)
        .push_next(&mut import_info)
        .push_next(&mut consumer_dedicated);
    let imported = unsafe { device.allocate_memory(&import_allocate_info, None) }
        .context("import tiled fd")?;
    unsafe { device.bind_image_memory(consumer_image, imported, 0) }
        .context("bind tiled consumer image")?;
    let consumer_plane = unsafe {
        device.get_image_subresource_layout(
            consumer_image,
            vk::ImageSubresource::default()
                .aspect_mask(vk::ImageAspectFlags::MEMORY_PLANE_0_EXT)
                .mip_level(0)
                .array_layer(0),
        )
    };
    let mut consumer_modifier = vk::ImageDrmFormatModifierPropertiesEXT::default();
    unsafe {
        ext_drm.get_image_drm_format_modifier_properties(consumer_image, &mut consumer_modifier)
    }
    .context("consumer modifier")?;
    println!(
        "consumer: imported fd, modifier {:#x}, plane layout offset={} size={} row_pitch={}",
        consumer_modifier.drm_format_modifier,
        consumer_plane.offset,
        consumer_plane.size,
        consumer_plane.row_pitch,
    );

    // Copy the consumer's view of the tiled image into a linear, host-visible buffer.
    let readback_size = (PIXELS * PIXELS * 4) as u64;
    let readback_buffer = unsafe {
        device.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(readback_size)
                .usage(vk::BufferUsageFlags::TRANSFER_DST),
            None,
        )
    }
    .context("readback buffer")?;
    let readback_requirements = unsafe { device.get_buffer_memory_requirements(readback_buffer) };
    let readback_memory_type_index = memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            readback_requirements.memory_type_bits & (1 << index) != 0
                && memory_type.property_flags.contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
        })
        .map(|(index, _)| index as u32)
        .context("no host-visible memory for readback")?;
    let readback_memory = unsafe {
        device.allocate_memory(
            &vk::MemoryAllocateInfo::default()
                .allocation_size(readback_requirements.size)
                .memory_type_index(readback_memory_type_index),
            None,
        )
    }
    .context("readback memory")?;
    unsafe { device.bind_buffer_memory(readback_buffer, readback_memory, 0) }
        .context("bind readback")?;

    // Sanity check: read the producer's *own* image back through the same copy path, so a mismatch
    // can be attributed to the import rather than to the fill.
    unsafe {
        device
            .begin_command_buffer(command_buffer, &vk::CommandBufferBeginInfo::default())
            .context("begin producer readback")?;
        let to_src = vk::ImageMemoryBarrier::default()
            .image(producer_image)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .subresource_range(subresource_range);
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_src],
        );
        device.cmd_copy_image_to_buffer(
            command_buffer,
            producer_image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            readback_buffer,
            &[region],
        );
        device
            .end_command_buffer(command_buffer)
            .context("end producer readback")?;
    }
    let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers);
    unsafe { device.queue_submit(queue, &[submit_info], vk::Fence::null()) }
        .context("submit producer readback")?;
    unsafe { device.queue_wait_idle(queue) }.context("wait producer readback")?;
    {
        let mapped = unsafe {
            device.map_memory(
                readback_memory,
                0,
                readback_size,
                vk::MemoryMapFlags::empty(),
            )
        }
        .context("map producer readback")? as *const u8;
        let bytes = unsafe { std::slice::from_raw_parts(mapped, readback_size as usize) };
        let matching = bytes.iter().zip(&pattern).filter(|(a, b)| a == b).count();
        println!(
            "  producer self-readback: {matching}/{} bytes match",
            pattern.len()
        );
        unsafe { device.unmap_memory(readback_memory) };
    }

    unsafe {
        device
            .begin_command_buffer(command_buffer, &vk::CommandBufferBeginInfo::default())
            .context("begin copy")?;
        let to_src = vk::ImageMemoryBarrier::default()
            .image(consumer_image)
            .old_layout(vk::ImageLayout::GENERAL)
            .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
            .subresource_range(subresource_range);
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_src],
        );
        device.cmd_copy_image_to_buffer(
            command_buffer,
            consumer_image,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            readback_buffer,
            &[region],
        );
        device
            .end_command_buffer(command_buffer)
            .context("end copy")?;
    }
    let submit_info = vk::SubmitInfo::default().command_buffers(&command_buffers);
    unsafe { device.queue_submit(queue, &[submit_info], vk::Fence::null()) }
        .context("submit copy")?;
    unsafe { device.queue_wait_idle(queue) }.context("wait copy")?;

    let mapped = unsafe {
        device.map_memory(
            readback_memory,
            0,
            readback_size,
            vk::MemoryMapFlags::empty(),
        )
    }
    .context("map readback")? as *const u8;
    let bytes = unsafe { std::slice::from_raw_parts(mapped, readback_size as usize) };
    let matching = bytes.iter().zip(&pattern).filter(|(a, b)| a == b).count();
    let first_diff = bytes.iter().zip(&pattern).position(|(a, b)| a != b);
    println!(
        "round trip: tiled VkImage {matching}/{} bytes match the producer's gradient{}",
        pattern.len(),
        if matching == pattern.len() {
            " — MATCH".to_string()
        } else {
            format!(" — MISMATCH at byte {first_diff:?}")
        },
    );
    unsafe { device.unmap_memory(readback_memory) };

    unsafe {
        device.destroy_image(producer_image, None);
        device.destroy_image(consumer_image, None);
        device.destroy_buffer(staging_buffer, None);
        device.destroy_buffer(readback_buffer, None);
        device.free_memory(memory, None);
        device.free_memory(imported, None);
        device.free_memory(staging_memory, None);
        device.free_memory(readback_memory, None);
        device.destroy_command_pool(command_pool, None);
        device.destroy_device(None);
    }
    Ok(())
}

/// Stages 2 and 3 — allocate a dma-buf on a raw Vulkan device (the "producer", which is not wgpu),
/// bind a linear `VkImage` over it, write known bytes, export the fd, then import that fd as a
/// second `VkImage` and read the bytes back unchanged.
fn run_dmabuf_round_trip(
    instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
    format: &Format,
) -> Result<()> {
    use ash::khr::external_memory_fd::Device as ExternalMemoryFd;
    use ash::vk;

    let properties = unsafe { instance.get_physical_device_properties(physical_device) };
    let name = c_char_slice_to_string(&properties.device_name);
    println!("=== dma-buf round trip on {name} ===");

    // A graphics queue family, so the device could also drive the renderer it feeds.
    let queue_families =
        unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let queue_family_index = queue_families
        .iter()
        .position(|family| family.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .context("no graphics queue family")? as u32;

    // The device, with the import extensions and `VK_EXT_image_drm_format_modifier` enabled, so
    // both the export/import commands and the tiled-modifier query exist.
    let extension_names = [
        vk::KHR_EXTERNAL_MEMORY_FD_NAME.as_ptr(),
        vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME.as_ptr(),
        vk::KHR_IMAGE_FORMAT_LIST_NAME.as_ptr(),
        vk::KHR_BIND_MEMORY2_NAME.as_ptr(),
        vk::EXT_IMAGE_DRM_FORMAT_MODIFIER_NAME.as_ptr(),
    ];
    let queue_priorities = [1.0f32];
    let queue_create_info = vk::DeviceQueueCreateInfo::default()
        .queue_family_index(queue_family_index)
        .queue_priorities(&queue_priorities);
    let queue_create_infos = [queue_create_info];
    let device_create_info = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queue_create_infos)
        .enabled_extension_names(&extension_names);
    let device = unsafe { instance.create_device(physical_device, &device_create_info, None) }
        .context("create device")?;
    let external_memory_fd = ExternalMemoryFd::new(instance, &device);

    // The tiled modifiers the driver offers, now that a device has enabled the extension the query
    // needs.
    if format.fourcc == "ABGR8888" {
        print_modifiers(instance, physical_device);
    }

    // The image the dma-buf will back: linear tiling, so the producer can map the memory and write
    // the bytes directly, and so a `DRM_FORMAT_MOD_LINEAR` import has a matching layout.
    let image_create_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format.vk_format)
        .extent(vk::Extent3D {
            width: PIXELS,
            height: PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::LINEAR)
        .usage(vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let producer_image = unsafe { device.create_image(&image_create_info, None) }
        .context("create producer image")?;
    let requirements = unsafe { device.get_image_memory_requirements(producer_image) };

    // A memory type the image can live in, and that the producer can also map and write.
    let memory_properties =
        unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let memory_type_index = memory_properties
        .memory_types
        .iter()
        .enumerate()
        .find(|(index, memory_type)| {
            requirements.memory_type_bits & (1 << index) != 0
                && memory_type.property_flags.contains(
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
        })
        .map(|(index, _)| index as u32)
        .context("no image-compatible host-visible memory type")?;
    let allocation_size = requirements.size;

    // --- producer: allocate a dma-buf, bind the image, write, export -----------------------
    let mut export_info = vk::ExportMemoryAllocateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(allocation_size)
        .memory_type_index(memory_type_index)
        .push_next(&mut export_info);
    let memory = unsafe { device.allocate_memory(&allocate_info, None) }
        .context("allocate export memory")?;
    unsafe { device.bind_image_memory(producer_image, memory, 0) }
        .context("bind producer image")?;

    // A solid, opaque colour, so the sample can verify one pixel and the map readback can verify the
    // whole buffer, in the format's own byte order.
    let known: Vec<u8> = (0..allocation_size)
        .map(|index| format.solid[(index % 4) as usize])
        .collect();
    unsafe {
        let mapped = device
            .map_memory(memory, 0, allocation_size, vk::MemoryMapFlags::empty())
            .context("map export memory")? as *mut u8;
        std::ptr::copy_nonoverlapping(known.as_ptr(), mapped, allocation_size as usize);
        device.unmap_memory(memory);
    }

    let get_fd_info = vk::MemoryGetFdInfoKHR::default()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let fd =
        unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }.context("export memory fd")?;
    println!(
        "producer: exported fd {fd}, {allocation_size} bytes, fourcc={} ({:?}), modifier=linear",
        format.fourcc, format.vk_format
    );

    // --- consumer: import the fd as a VkImage, read back, verify ------------------------
    let mut import_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(fd);
    let import_allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(allocation_size)
        .memory_type_index(memory_type_index)
        .push_next(&mut import_info);
    let imported = unsafe { device.allocate_memory(&import_allocate_info, None) }
        .context("import memory fd")?;
    let consumer_image = unsafe { device.create_image(&image_create_info, None) }
        .context("create consumer image")?;
    unsafe { device.bind_image_memory(consumer_image, imported, 0) }
        .context("bind consumer image")?;
    println!(
        "consumer: imported fd {fd} as a {PIXELS}x{PIXELS} linear {:?} image",
        format.vk_format
    );

    let mut read_back = vec![0u8; allocation_size as usize];
    unsafe {
        let mapped = device
            .map_memory(imported, 0, allocation_size, vk::MemoryMapFlags::empty())
            .context("map imported memory")? as *const u8;
        std::ptr::copy_nonoverlapping(mapped, read_back.as_mut_ptr(), allocation_size as usize);
        device.unmap_memory(imported);
    }

    if read_back == known {
        println!("round trip: {allocation_size} bytes match through a VkImage, no CPU copy");
    } else {
        let first_diff = read_back.iter().zip(&known).position(|(a, b)| a != b);
        println!("round trip: MISMATCH at byte {first_diff:?}");
    }

    // A second export for the wgpu adoption: the first fd was consumed by the raw import above.
    let get_fd_info = vk::MemoryGetFdInfoKHR::default()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
        .context("export memory fd (2)")?;
    run_wgpu_adoption(fd, allocation_size, memory_type_index, format)?;

    unsafe {
        device.destroy_image(producer_image, None);
        device.destroy_image(consumer_image, None);
        device.free_memory(memory, None);
        device.free_memory(imported, None);
        device.destroy_device(None);
    }
    Ok(())
}

/// The device-name and extension-name fields are NUL-terminated `[c_char; N]` arrays; take the
/// bytes up to the NUL and treat them as UTF-8, lossily.
fn c_char_slice_to_string(chars: &[c_char]) -> String {
    let length = chars
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(chars.len());
    let bytes: Vec<u8> = chars[..length].iter().map(|&byte| byte as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Stage 3b — the consumer that matters: import the fd on the renderer's own device, adopt the
/// resulting `VkImage` into wgpu (`texture_from_raw` + `create_texture_from_hal`), and sample it
/// through a pipeline. This is the adoption `shared-surface.md` §1 reads on every backend but has
/// only ever run on Windows.
fn run_wgpu_adoption(
    fd: i32,
    allocation_size: u64,
    memory_type_index: u32,
    format: &Format,
) -> Result<()> {
    use ash::vk;

    // The renderer's own device, created the way `gpui_wgpu` would create it.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
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
    .context("no wgpu adapter")?;
    let adapter_info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("linux-dmabuf-consumer"),
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::downlevel_defaults(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }))
    .context("no wgpu device")?;

    // The renderer's raw Vulkan device, for the import the hal API does not expose.
    let hal_device = unsafe { device.as_hal::<wgpu::hal::vulkan::Api>() }
        .context("the wgpu adapter is not Vulkan")?;
    let raw_device = hal_device.raw_device();

    // Import the fd as a linear VkImage on the renderer's device.
    let image_create_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format.vk_format)
        .extent(vk::Extent3D {
            width: PIXELS,
            height: PIXELS,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::LINEAR)
        .usage(vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let image = unsafe { raw_device.create_image(&image_create_info, None) }
        .context("create wgpu-consumer image")?;

    let mut import_info = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(fd);
    let import_allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(allocation_size)
        .memory_type_index(memory_type_index)
        .push_next(&mut import_info);
    let imported_memory = unsafe { raw_device.allocate_memory(&import_allocate_info, None) }
        .context("import memory on the wgpu device")?;
    unsafe { raw_device.bind_image_memory(image, imported_memory, 0) }
        .context("bind wgpu-consumer image")?;

    // Adopt the raw image into a wgpu texture.
    let hal_texture = unsafe {
        hal_device.texture_from_raw(
            image,
            &wgpu::hal::TextureDescriptor {
                label: Some("dma-buf import"),
                size: wgpu::Extent3d {
                    width: PIXELS,
                    height: PIXELS,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.wgpu_format,
                usage: wgpu::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            },
            None,
            wgpu::hal::vulkan::TextureMemory::Dedicated(imported_memory),
        )
    };
    let texture = unsafe {
        device.create_texture_from_hal::<wgpu::hal::vulkan::Api>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("dma-buf import"),
                size: wgpu::Extent3d {
                    width: PIXELS,
                    height: PIXELS,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.wgpu_format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
        )
    };
    println!(
        "consumer: adopted fd {fd} into a wgpu texture on {:?}",
        adapter_info.name
    );

    // Sample the texture through a compute shader and read one pixel back.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sample"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@compute @workgroup_size(1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x == 0u && gid.y == 0u {
        output[0] = textureSampleLevel(tex, samp, vec2<f32>(0.5, 0.5), 0.0);
    }
}
"#
            .into(),
        ),
    });

    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sample output"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sample readback"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("sample pipeline"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind_group_layout = pipeline.get_bind_group_layout(0);
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("sample bind group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("sample"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("sample"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, 16);
    queue.submit([encoder.finish()]);

    let slice = readback_buffer.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|error| anyhow::anyhow!("poll: {error}"))?;
    receiver.recv().expect("readback channel")?;
    let mapped = slice.get_mapped_range();
    let rgba = [
        f32::from_le_bytes([mapped[0], mapped[1], mapped[2], mapped[3]]),
        f32::from_le_bytes([mapped[4], mapped[5], mapped[6], mapped[7]]),
        f32::from_le_bytes([mapped[8], mapped[9], mapped[10], mapped[11]]),
        f32::from_le_bytes([mapped[12], mapped[13], mapped[14], mapped[15]]),
    ];
    let expected = [32.0 / 255.0, 192.0 / 255.0, 64.0 / 255.0, 1.0];
    println!(
        "sample: {rgba:?} — expected {expected:?} — {}",
        if (0..4).all(|i| (rgba[i] - expected[i]).abs() < 0.01) {
            "MATCH"
        } else {
            "MISMATCH"
        }
    );

    // wgpu now owns both the image and the imported memory: `drop_callback` is `None` (so the image
    // is destroyed on drop) and the memory is `Dedicated` (so it is freed on drop). Dropping the
    // texture and its views at the end of this function is what tears them down.
    Ok(())
}
