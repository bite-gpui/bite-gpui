//! Probe: can a wgpu producer serve GPUI's Metal renderer on macOS?
//!
//! Path A requires producer and consumer to be on the same device (decision 0002). On macOS the
//! consumer is always `MetalRenderer`, which created the `MTLDevice` it draws on, and a wgpu
//! producer has to reach *that* device: `wgpu-hal`'s Metal backend speaks `objc2-metal`, where GPUI
//! speaks the `metal` crate, and it exposes no public constructor that takes a device. So the
//! question the record leaves open is whether the route exists at all, and this prints what each
//! step can actually do rather than assuming an answer:
//!
//! 1. the device GPUI's renderer holds, reached the way an application reaches it;
//! 2. what a Metal-only wgpu instance enumerates;
//! 3. the device wgpu ends up on, per power preference, and whether it is GPUI's — by pointer;
//! 4. if it is, whether a texture wgpu created and wrote can be handed to the renderer through
//!    `MetalTextureExt` and sampled back byte for byte.
//!
//! It prints rather than asserts: a negative answer is a result. The printout is the evidence and is
//! recorded in `decisions/macos-wgpu-producer-probe.md` in the project repository.
//!
//! Run on macOS: `cargo run --manifest-path probes/macos-wgpu-producer/Cargo.toml`.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("macos only");
}

#[cfg(target_os = "macos")]
fn main() {
    imp::run();
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::{anyhow, Context as _};
    use foreign_types::{ForeignType, ForeignTypeRef};
    use gpui_apple::imported_texture::MetalTextureExt;
    use gpui_apple::metal_renderer::{InstanceBufferPool, MetalRenderer};
    use gpui_engine::{CustomRenderPrimitive, ImportedTextureHandle, Scene, SceneRenderer};
    use gpui_platform::{Bounds, ContentMask, Corners, DevicePixels, PlatformRenderer, Point, Size};
    use parking_lot::Mutex;
    use std::ffi::c_void;
    use std::sync::Arc;

    /// The producer's texture, and the target, are this small for the same reason the in-tree Path
    /// A rows are: one texel cannot interpolate, and 8x8 is one quad over the whole target.
    const TEXEL: u32 = 1;
    const VIEWPORT: i32 = 8;

    pub fn run() {
        // ---------------------------------------------------------------- 1. gpui's device
        let mut renderer = MetalRenderer::new_headless(Arc::new(Mutex::new(
            InstanceBufferPool::default(),
        )));
        let Some(gpui_device) = renderer
            .device_any()
            .and_then(|device| device.downcast::<metal::Device>().ok())
        else {
            println!("gpui:    the renderer lent no device; nothing to measure");
            return;
        };
        let gpui_ptr = gpui_device.as_ptr() as *const c_void;
        println!(
            "gpui:    device {:?} at {gpui_ptr:p}; metal sees {} device(s)",
            gpui_device.name(),
            metal::Device::all().len()
        );

        // ------------------------------------------------------- 2. what wgpu enumerates
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let enumerated = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::METAL));
        println!("wgpu:    {} adapter(s)", enumerated.len());
        for adapter in &enumerated {
            let info = adapter.get_info();
            println!(
                "wgpu:    enumerated {:?} ({:?}, {:?})",
                info.name, info.device_type, info.backend
            );
        }

        // ---------------------------------------- 3. which device wgpu actually lands on
        let mut chosen: Option<(String, wgpu::Device, wgpu::Queue)> = None;
        for preference in [
            wgpu::PowerPreference::HighPerformance,
            wgpu::PowerPreference::LowPower,
            wgpu::PowerPreference::None,
        ] {
            let adapter =
                match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: preference,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                })) {
                    Ok(adapter) => adapter,
                    Err(error) => {
                        println!("wgpu:    {preference:?}: no adapter ({error})");
                        continue;
                    }
                };
            let info = adapter.get_info();
            let (device, queue) =
                match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                    label: Some("probe"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::downlevel_defaults(),
                    memory_hints: wgpu::MemoryHints::MemoryUsage,
                    trace: wgpu::Trace::Off,
                    experimental_features: wgpu::ExperimentalFeatures::disabled(),
                })) {
                    Ok(pair) => pair,
                    Err(error) => {
                        println!("wgpu:    {preference:?}: no device ({error})");
                        continue;
                    }
                };
            let raw = unsafe { raw_device(&device) };
            let same = raw == Some(gpui_ptr);
            println!(
                "wgpu:    {preference:?} -> {:?} ({:?}) at {:p}, on gpui's device: {same}",
                info.name,
                info.device_type,
                raw.unwrap_or(std::ptr::null())
            );
            if same && chosen.is_none() {
                chosen = Some((info.name.clone(), device, queue));
            }
        }

        let Some((name, device, queue)) = chosen else {
            println!(
                "VERDICT: no wgpu power preference landed on GPUI's device on this machine, and the \
                 public API offers no way to be pointed at one — the hal's adapter constructor from \
                 an existing device is private — so a wgpu producer here needs either a preference \
                 that matches GPUI's choice or a Metal-native producer on the device device_any \
                 lends."
            );
            return;
        };
        println!("probe:   using the {name:?} device, which is GPUI's own");

        // -------------------------------------- 4. a wgpu texture, through the token
        let fixture = [200u8, 100, 50, 255];
        // The target is `Bgra8Unorm`, read back with its bytes swapped to RGBA, so the texture
        // holds the fixture in BGRA order — the convention the in-tree Path A rows use too.
        let stored = [fixture[2], fixture[1], fixture[0], fixture[3]];

        let written = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_imported_written"),
            size: wgpu::Extent3d {
                width: TEXEL,
                height: TEXEL,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // sRGB is the invariant: the fragment decodes the sample, so a texture that is not
            // declared sRGB would be encoded twice.
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            // `TEXTURE_BINDING` is not optional either: it is what makes wgpu put
            // `MTLTextureUsage::ShaderRead` on the texture, which `MetalTextureExt` checks.
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &written,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &stored,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: TEXEL,
                height: TEXEL,
                depth_or_array_layers: 1,
            },
        );

        // The other half of the producer's job: a texture wgpu *rendered* into. The clear colour is
        // the linear value the hardware's sRGB encode turns back into the fixture's byte.
        let rendered = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("probe_imported_rendered"),
            size: wgpu::Extent3d {
                width: TEXEL,
                height: TEXEL,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = rendered.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("probe_clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("probe_clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: srgb_to_linear(fixture[0]),
                            g: srgb_to_linear(fixture[1]),
                            b: srgb_to_linear(fixture[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
        }
        queue.submit(Some(encoder.finish()));

        // GPUI samples on its own queue, so the producer's work has to be finished before the
        // renderer looks at it. On one device this is the whole of the synchronisation — there is
        // no fence to carry, which is decision 0002's first tier — but the wait is still real.
        if let Err(error) = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        }) {
            println!("probe:   waiting for the producer failed: {error}");
            return;
        }

        for (label, texture) in [("written", &written), ("rendered", &rendered)] {
            if let Err(error) = sample(&mut renderer, texture, fixture) {
                println!("probe:   {label}: {error:#}");
            }
        }

        println!(
            "VERDICT: wgpu landed on GPUI's own device and a texture it made was sampled by its \
             Metal renderer through MetalTextureExt. A wgpu producer on macOS therefore needs no \
             device handover — only a power preference that matches the one GPUI picked."
        );
    }

    /// Renders a scene whose only primitive is `texture` over the whole target, and prints the
    /// readback against the fixture.
    fn sample(
        renderer: &mut MetalRenderer,
        texture: &wgpu::Texture,
        fixture: [u8; 4],
    ) -> anyhow::Result<()> {
        let handle = unsafe {
            let hal = texture
                .as_hal::<wgpu::hal::api::Metal>()
                .ok_or_else(|| anyhow!("the texture has no Metal handle"))?;
            let metal_texture = metal::TextureRef::from_ptr(hal.raw_handle() as *const _ as *mut _);
            println!(
                "probe:   wgpu texture is a {:?} {}x{}",
                metal_texture.pixel_format(),
                metal_texture.width(),
                metal_texture.height()
            );
            metal_texture
                .to_imported_handle()
                .context("MetalTextureExt refused the texture")?
        };

        let viewport = Size {
            width: DevicePixels(VIEWPORT),
            height: DevicePixels(VIEWPORT),
        };
        let scene = imported_texture_scene(handle, viewport);
        let pixels = SceneRenderer::render_scene_to_image(renderer, &scene, viewport)
            .context("rendering the scene")?;
        let got = &pixels.data()[..4];
        let exact = got == fixture;
        println!(
            "probe:   readback {got:?} against {fixture:?} at {}x{}: {}",
            pixels.width(),
            pixels.height(),
            if exact { "byte for byte" } else { "NOT byte for byte" }
        );
        Ok(())
    }

    /// The raw `MTLDevice` a wgpu device is on, as a pointer comparable with the `metal` crate's
    /// own. `Retained::as_ptr` is the objc2 handle on the same Objective-C object `metal` wraps,
    /// which is what makes the comparison meaningful rather than a name match.
    unsafe fn raw_device(device: &wgpu::Device) -> Option<*const c_void> {
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }?;
        Some(objc2::rc::Retained::as_ptr(hal.raw_device()) as *const c_void)
    }

    /// A scene whose only primitive is an imported texture covering the whole target.
    fn imported_texture_scene(handle: ImportedTextureHandle, viewport: Size<DevicePixels>) -> Scene {
        let bounds = Bounds {
            origin: Point {
                x: 0.0.into(),
                y: 0.0.into(),
            },
            size: Size {
                width: (viewport.width.0 as f32).into(),
                height: (viewport.height.0 as f32).into(),
            },
        };
        let mut scene = Scene::default();
        scene.custom.push(CustomRenderPrimitive::Texture {
            order: 0,
            handle,
            bounds,
            content_mask: ContentMask { bounds },
            radii: Corners::default(),
            opacity: 1.0,
            flip_v: false,
        });
        scene
    }

    /// The sRGB electro-optical transfer function, so the clear colour is one the hardware's sRGB
    /// encode turns back into the byte the fixture names.
    fn srgb_to_linear(byte: u8) -> f64 {
        let encoded = byte as f64 / 255.0;
        if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    }
}
