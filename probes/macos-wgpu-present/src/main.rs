//! Probe for `spi/rendering/spike-macos-presentation.md`: can `wgpu` present on the `NSView`
//! a GPUI window is created with, and whose `CAMetalLayer` is the view's backing layer?
//!
//! Probe 1 builds a view whose layer the caller installed — what GPUI's
//! `-[NSView makeBackingLayer]` does, by returning the renderer's — and presents from wgpu on
//! it. Probe 2 gives the view a plain `CALayer`, and then no layer of its own, which the
//! reading says fails. Probe 3 asks what `gpui_wgpu`'s backend set finds here. Probe 4 tries a
//! surface before any layer exists and then installs one on the same view, which is the
//! ordering question patch 06 turns on.

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
    use std::any::Any;
    use std::ffi::c_void;
    use std::panic::AssertUnwindSafe;

    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSRect, NSSize, NSPoint};
    use objc2_quartz_core::{CALayer, CAMetalLayer};
    use wgpu::rwh::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};

    const WIDTH: u32 = 512;
    const HEIGHT: u32 = 512;

    fn frame() -> NSRect {
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(WIDTH as f64, HEIGHT as f64),
        )
    }

    /// A `CAMetalLayer` sized like the view, so `wgpu`'s `dimensions()` reads something.
    fn metal_layer() -> Retained<CAMetalLayer> {
        let layer = CAMetalLayer::new();
        layer.setBounds(frame());
        layer.setContentsScale(1.0);
        layer
    }

    fn raw_handles(view: &Retained<NSView>) -> (RawWindowHandle, RawDisplayHandle) {
        let pointer = Retained::as_ptr(view) as *mut c_void;
        let ns_view = std::ptr::NonNull::new(pointer).expect("a live NSView");
        (
            RawWindowHandle::AppKit(AppKitWindowHandle::new(ns_view)),
            RawDisplayHandle::AppKit(AppKitDisplayHandle::new()),
        )
    }

    fn instance_for(backends: wgpu::Backends) -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        })
    }

    fn describe(status: wgpu::CurrentSurfaceTexture) -> &'static str {
        use wgpu::CurrentSurfaceTexture as Status;
        match status {
            Status::Success(_) => "Success",
            Status::Suboptimal(_) => "Suboptimal",
            Status::Timeout => "Timeout",
            Status::Occluded => "Occluded",
            Status::Outdated => "Outdated",
            Status::Lost => "Lost",
            Status::Validation => "Validation",
        }
    }

    fn panic_message(payload: Box<dyn Any + Send>) -> String {
        if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_string()
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            "<non-string panic>".to_string()
        }
    }

    /// `raw_window_metal` reads the view's layer and asserts its kind, so a view without a
    /// `CAMetalLayer` panics rather than returning an error. Keep the probe alive across it.
    fn attempt<T>(label: &str, f: impl FnOnce() -> T) -> Option<T> {
        match std::panic::catch_unwind(AssertUnwindSafe(f)) {
            Ok(value) => Some(value),
            Err(payload) => {
                println!("{label}: PANICKED: {}", panic_message(payload));
                None
            }
        }
    }

    /// Build a surface from the view, find an adapter, describe the surface, present a frame.
    fn present_one_frame(view: &Retained<NSView>, backends: wgpu::Backends, label: &str) {
        println!("\n--- {label}: {backends:?} ---");
        let instance = instance_for(backends);
        let (raw_window_handle, raw_display_handle) = raw_handles(view);

        let created = attempt(label, || unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })
        });
        let surface = match created {
            None => return,
            Some(Ok(surface)) => surface,
            Some(Err(error)) => {
                println!("{label}: surface NOT created: {error}");
                return;
            }
        };
        println!("{label}: surface created from the view");

        let adapter =
            match pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })) {
                Ok(adapter) => adapter,
                Err(error) => {
                    println!("{label}: NO ADAPTER: {error}");
                    return;
                }
            };
        let info = adapter.get_info();
        println!(
            "{label}: adapter = {:?}, backend = {:?}, device_type = {:?}",
            info.name, info.backend, info.device_type
        );

        let capabilities = surface.get_capabilities(&adapter);
        println!("{label}: formats = {:?}", capabilities.formats);
        println!("{label}: present_modes = {:?}", capabilities.present_modes);
        println!("{label}: alpha_modes = {:?}", capabilities.alpha_modes);

        let (device, queue) =
            match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("bite macos present probe"),
                ..Default::default()
            })) {
                Ok(pair) => pair,
                Err(error) => {
                    println!("{label}: request_device FAILED: {error}");
                    return;
                }
            };

        let Some(format) = capabilities
            .formats
            .iter()
            .find(|format| format.is_srgb())
            .or_else(|| capabilities.formats.first())
            .copied()
        else {
            println!("{label}: the surface reports no format");
            return;
        };
        let Some(alpha_mode) = capabilities.alpha_modes.first().copied() else {
            println!("{label}: the surface reports no alpha mode");
            return;
        };

        surface.configure(
            &device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: WIDTH,
                height: HEIGHT,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode,
                view_formats: Vec::new(),
            },
        );
        println!("{label}: configured format = {format:?}, alpha_mode = {alpha_mode:?}");

        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            other => {
                println!("{label}: get_current_texture -> {}", describe(other));
                return;
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bite macos present probe"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("probe clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 1.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit([encoder.finish()]);
        frame.present();
        println!("{label}: PRESENT OK");
    }

    pub fn run() {
        println!("macos-wgpu-present probe");
        let Some(mtm) = MainThreadMarker::new() else {
            println!("not on the main thread; nothing can be built");
            return;
        };
        println!("main-thread marker acquired");

        let instance = instance_for(wgpu::Backends::all());
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        if adapters.is_empty() {
            println!("machine adapters: none at all (this is itself a finding)");
        }
        for adapter in adapters {
            let info = adapter.get_info();
            println!(
                "machine adapter: {:?}, backend = {:?}, device_type = {:?}",
                info.name, info.backend, info.device_type
            );
        }

        // Probe 1: the layer the caller installed, which is what GPUI's view returns.
        let installed = NSView::new(mtm);
        let layer = metal_layer();
        installed.setWantsLayer(true);
        installed.setLayer(Some(&layer));
        println!("\nprobe 1: the view's layer is a CAMetalLayer");
        present_one_frame(
            &installed,
            wgpu::Backends::METAL,
            "METAL, layer installed by the caller",
        );
        present_one_frame(
            &installed,
            wgpu::Backends::VULKAN | wgpu::Backends::GL,
            "gpui_wgpu's set (VULKAN | GL)",
        );
        present_one_frame(&installed, wgpu::Backends::VULKAN, "VULKAN");
        present_one_frame(&installed, wgpu::Backends::GL, "GL");

        // Probe 2: a layer that is not a CAMetalLayer, and then none of the view's own.
        let plain = NSView::new(mtm);
        plain.setWantsLayer(true);
        plain.setLayer(Some(&CALayer::new()));
        println!("\nprobe 2: the view's layer is a plain CALayer");
        present_one_frame(&plain, wgpu::Backends::METAL, "METAL, plain CALayer");

        let bare = NSView::new(mtm);
        println!("probe 2: the view has no layer of its own");
        present_one_frame(&bare, wgpu::Backends::METAL, "METAL, no layer");

        // Probe 4: the same view, before and after a CAMetalLayer arrives.
        println!("\nprobe 4: the ordering");
        let late = NSView::new(mtm);
        present_one_frame(&late, wgpu::Backends::METAL, "METAL, no layer yet");
        let layer = metal_layer();
        late.setWantsLayer(true);
        late.setLayer(Some(&layer));
        println!("probe 4: CAMetalLayer installed on the same view");
        present_one_frame(&late, wgpu::Backends::METAL, "METAL, after the layer arrived");
    }
}
