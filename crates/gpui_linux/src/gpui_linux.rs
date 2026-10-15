#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;

/// The producer's half of Path A where wgpu is the renderer, which on Linux is the default one.
/// Re-exported here for the same reason gpui_macos re-exports Metal's: this is the crate an
/// application already reaches the platform through, so it does not have to know that the renderer
/// lives in a crate of its own.
#[cfg(any(feature = "wayland", feature = "x11"))]
pub use gpui_wgpu::ImportedTextureExt;

/// The Linux surface transport, re-exported so an application reaches it through the platform crate
/// it already depends on rather than through the engine.
#[cfg(target_os = "linux")]
pub use gpui_engine::{DmaBufFormat, DmaBufHandle, DmaBufPlane};
