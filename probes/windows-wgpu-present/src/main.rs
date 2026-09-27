//! Probe for `spi/rendering/spike-windows-presentation.md`: can `wgpu` present on the
//! `HWND` a GPUI window is created with, and on which backend?
//!
//! Probe 1 prints, for each backend set, whether a surface can be built from the raw
//! handle, whether an adapter is found, what the surface reports, and whether a frame can
//! be acquired and presented. `VULKAN | GL` is the set `gpui_wgpu` asks for today
//! (`crates/gpui_wgpu/src/wgpu_context.rs:292`); `DX12` is the one Windows would want.
//!
//! Probe 2 commits a DirectComposition target and visual on the same `HWND` — the tree
//! `gpui_windows` drives when composition is on — and then presents from wgpu on it,
//! which is the question of whether the window has to stop owning a swap chain.

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("windows only");
}

#[cfg(target_os = "windows")]
fn main() {
    imp::run();
}

#[cfg(target_os = "windows")]
mod imp {
    use std::num::NonZeroIsize;

    use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, Win32WindowHandle, WindowsDisplayHandle};
    use windows::core::{w, Interface};
    use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL_11_0,
    };
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    };
    use windows::Win32::Graphics::DirectComposition::{
        DCompositionCreateDevice, IDCompositionDevice,
    };
    use windows::Win32::Graphics::Dxgi::Common::{
        DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
    };
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory2, IDXGIDevice, IDXGIFactory2, DXGI_SCALING_STRETCH,
        DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, RegisterClassW, ShowWindow, SW_SHOW, WINDOW_EX_STYLE,
        WNDCLASSW, WS_OVERLAPPEDWINDOW,
    };

    const WIDTH: u32 = 512;
    const HEIGHT: u32 = 512;
    const BUFFER_COUNT: u32 = 2;

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    fn create_window() -> windows::core::Result<HWND> {
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class = w!("BiteWgpuPresentProbe");
            let class_struct = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            // A zero return means the class already exists, which is fine.
            RegisterClassW(&class_struct);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!("bite wgpu present probe"),
                WS_OVERLAPPEDWINDOW,
                0,
                0,
                WIDTH as i32,
                HEIGHT as i32,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?;
            let _ = ShowWindow(hwnd, SW_SHOW);
            Ok(hwnd)
        }
    }

    fn raw_handles(hwnd: HWND) -> (RawWindowHandle, RawDisplayHandle) {
        let hwnd = NonZeroIsize::new(hwnd.0 as isize).expect("HWND must not be null");
        (
            RawWindowHandle::Win32(Win32WindowHandle::new(hwnd)),
            RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
        )
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

    /// Build a surface from the window's raw handle, find an adapter, describe the
    /// surface, and present one cleared frame.
    fn present_one_frame(hwnd: HWND, backends: wgpu::Backends, label: &str) {
        println!("\n--- {label}: {backends:?} ---");
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let (raw_window_handle, raw_display_handle) = raw_handles(hwnd);
        let surface = match unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            })
        } {
            Ok(surface) => surface,
            Err(error) => {
                println!("{label}: surface NOT created: {error}");
                return;
            }
        };
        println!("{label}: surface created from the raw handle");

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
                label: Some("bite wgpu present probe"),
                ..Default::default()
            })) {
                Ok(device_and_queue) => device_and_queue,
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

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bite wgpu present probe"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("probe clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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

    /// A D3D11 device the way `gpui_windows` has one: BGRA support, composition-capable.
    fn create_d3d11_device() -> windows::core::Result<ID3D11Device> {
        for (driver_type, name) in [
            (D3D_DRIVER_TYPE_HARDWARE, "hardware"),
            (D3D_DRIVER_TYPE_WARP, "warp"),
        ] {
            let mut device: Option<ID3D11Device> = None;
            let result = unsafe {
                D3D11CreateDevice(
                    None,
                    driver_type,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
            };
            match (result, device) {
                (Ok(()), Some(device)) => {
                    println!("probe 2: D3D11 device created ({name})");
                    return Ok(device);
                }
                (Err(error), _) => println!("probe 2: D3D11 {name} device failed: {error}"),
                (Ok(()), None) => println!("probe 2: D3D11 {name} device returned nothing"),
            }
        }
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_FAIL,
        ))
    }

    /// Does GPUI's composition tree on the `HWND` interfere with a wgpu surface on it?
    fn probe_direct_composition(hwnd: HWND) -> windows::core::Result<()> {
        println!("\n--- probe 2: a DirectComposition tree on the HWND, then wgpu ---");
        let device = create_d3d11_device()?;
        let dxgi_device: IDXGIDevice = device.cast()?;
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory2(Default::default())? };

        let description = DXGI_SWAP_CHAIN_DESC1 {
            Width: WIDTH,
            Height: HEIGHT,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: BUFFER_COUNT,
            // Composition swap chains only support the stretch scaling.
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        let swap_chain =
            unsafe { factory.CreateSwapChainForComposition(&device, &description, None)? };
        println!("probe 2: composition swap chain created");

        let composition_device: IDCompositionDevice =
            unsafe { DCompositionCreateDevice(&dxgi_device)? };
        let target = unsafe { composition_device.CreateTargetForHwnd(hwnd, true)? };
        let visual = unsafe { composition_device.CreateVisual()? };
        unsafe {
            visual.SetContent(&swap_chain)?;
            target.SetRoot(&visual)?;
            composition_device.Commit()?;
        }
        println!("probe 2: DirectComposition target, visual and swap chain committed");

        present_one_frame(hwnd, wgpu::Backends::DX12, "DX12 with the dcomp tree live");
        Ok(())
    }

    pub fn run() {
        println!("windows-wgpu-present probe");
        let hwnd = match create_window() {
            Ok(hwnd) => hwnd,
            Err(error) => {
                println!("window creation FAILED: {error}");
                return;
            }
        };
        println!("window created: {hwnd:?}");

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
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

        present_one_frame(
            hwnd,
            wgpu::Backends::VULKAN | wgpu::Backends::GL,
            "gpui_wgpu's set (VULKAN | GL)",
        );
        present_one_frame(hwnd, wgpu::Backends::DX12, "DX12");
        present_one_frame(hwnd, wgpu::Backends::VULKAN, "VULKAN");
        present_one_frame(hwnd, wgpu::Backends::GL, "GL");

        if let Err(error) = probe_direct_composition(hwnd) {
            println!("probe 2 FAILED: {error}");
        }
    }
}
