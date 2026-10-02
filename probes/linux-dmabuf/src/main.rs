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

    // A solid, opaque colour, so the sample can verify one pixel and the map readback can verify the
    // whole buffer: [R, G, B, A] repeated.
    const SOLID: [u8; 4] = [32, 192, 64, 255];
    let known: Vec<u8> = (0..allocation_size)
        .map(|index| SOLID[(index % 4) as usize])
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

    // A second export for the wgpu adoption: the first fd was consumed by the raw import above.
    let get_fd_info = vk::MemoryGetFdInfoKHR::default()
        .memory(memory)
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let fd = unsafe { external_memory_fd.get_memory_fd(&get_fd_info) }
        .context("export memory fd (2)")?;
    run_wgpu_adoption(fd, allocation_size, memory_type_index)?;

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
fn run_wgpu_adoption(fd: i32, allocation_size: u64, memory_type_index: u32) -> Result<()> {
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
                format: wgpu::TextureFormat::Rgba8Unorm,
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
                format: wgpu::TextureFormat::Rgba8Unorm,
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
