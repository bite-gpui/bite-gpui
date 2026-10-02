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
        Some(&device) => run_dmabuf_round_trip(&instance, device)?,
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

/// Stages 2 and 3 — allocate a dma-buf on a raw Vulkan device (the "producer", which is not wgpu),
/// bind a linear `VkImage` over it, write known bytes, export the fd, then import that fd as a
/// second `VkImage` and read the bytes back unchanged.
fn run_dmabuf_round_trip(
    instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
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

    // The device, with the four import extensions enabled so the export/import commands exist.
    let extension_names = [
        vk::KHR_EXTERNAL_MEMORY_FD_NAME.as_ptr(),
        vk::EXT_EXTERNAL_MEMORY_DMA_BUF_NAME.as_ptr(),
        vk::KHR_IMAGE_FORMAT_LIST_NAME.as_ptr(),
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
        .context("create device")?;
    let external_memory_fd = ExternalMemoryFd::new(instance, &device);

    // The image the dma-buf will back: linear tiling, so the producer can map the memory and write
    // the bytes directly, and so a `DRM_FORMAT_MOD_LINEAR` import has a matching layout.
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

    let known: Vec<u8> = (0..allocation_size)
        .map(|index| ((index as u32).wrapping_mul(7).wrapping_add(13)) as u8)
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
        "producer: exported fd {fd}, {allocation_size} bytes, fourcc=ABGR8888 (R8G8B8A8_UNORM), modifier=linear"
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
    println!("consumer: imported fd {fd} as a {PIXELS}x{PIXELS} linear R8G8B8A8_UNORM image");

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
