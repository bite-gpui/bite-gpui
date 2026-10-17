#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;

/// The debug GPU override as a `GpuPreference`, for the headless entry point that has no platform
/// instance to ask.
///
/// `ZED_DEVICE_ID` is read here, in the platform, so neither the renderer nor `gpui_platform` knows
/// the variable. A windowed application reaches the same value through `set_gpu_preference`; the
/// headless renderer has no platform, so `gpui::current_headless_renderer` calls this instead.
pub use linux::gpu_preference_from_env;

/// The producer's half of the imported-texture path where wgpu is the renderer, which on Linux is the default one.
/// Re-exported here for the same reason gpui_macos re-exports Metal's: this is the crate an
/// application already reaches the platform through, so it does not have to know that the renderer
/// lives in a crate of its own.
#[cfg(any(feature = "wayland", feature = "x11"))]
pub use gpui_wgpu::ImportedTextureExt;

/// The headless renderer, so `gpui::current_headless_renderer` can build one on this platform the way
/// it builds Metal's on macOS. The Linux platform owns no renderer of its own; wgpu is the renderer a
/// window uses here, so a headless context renders through the same code.
#[cfg(all(
    feature = "gpui_wgpu",
    any(feature = "bench-support", feature = "test-support")
))]
pub use gpui_wgpu::WgpuHeadlessRenderer;

/// The Linux surface transport, re-exported so an application reaches it through the platform crate
/// it already depends on rather than through the engine.
#[cfg(target_os = "linux")]
pub use gpui_engine::{DmaBufHandle, DmaBufPlane, SurfaceFormat, SurfaceFormatKind};
