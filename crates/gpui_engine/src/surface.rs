//! The content a [`PaintSurface`](crate::PaintSurface) draws, including the native handles
//! the platform backends hand to their renderers.
//!
//! `SurfaceSource` travels on the scene (it is the payload of
//! [`PaintSurface::source`](crate::PaintSurface)), so it lives beside the scene rather than
//! above it, and the `From` conversions for the platform handle types live with it.
//! `WindowsScreenCaptureFrame` is the Windows payload it carries.

use gpui_types::{DevicePixels, Size};
#[cfg(target_os = "windows")]
use std::sync::Arc;
#[cfg(target_os = "macos")]
use core_video::pixel_buffer::CVPixelBuffer;
#[cfg(target_os = "windows")]
use windows_061::Win32::Graphics::Direct3D11::ID3D11Texture2D;

/// A source of a surface's content.
#[derive(Clone)]
pub enum SurfaceSource {
    /// A macOS image buffer from CoreVideo
    #[cfg(target_os = "macos")]
    Surface(CVPixelBuffer),
    /// A GPU texture handle (type-erased to avoid depending on wgpu)
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        all(target_family = "wasm", feature = "custom-gpu")
    ))]
    Texture {
        /// The GPU texture, type-erased (expected to be `Arc<wgpu::Texture>`)
        #[cfg(not(target_family = "wasm"))]
        texture: std::sync::Arc<dyn std::any::Any + Send + Sync>,
        /// The GPU texture, type-erased (expected to be `Arc<wgpu::Texture>`).
        ///
        /// WGPU handles are intentionally thread-local in browser builds.
        #[cfg(target_family = "wasm")]
        texture: std::sync::Arc<dyn std::any::Any>,
        /// Dimensions of the texture in device pixels
        size: Size<DevicePixels>,
    },
    /// A native Windows Graphics Capture texture.
    #[cfg(target_os = "windows")]
    WindowsCapture(WindowsScreenCaptureFrame),
    /// A placeholder for platforms that cannot import native surfaces.
    #[doc(hidden)]
    Unsupported(Size<DevicePixels>),
}

impl std::fmt::Debug for SurfaceSource {
    fn fmt(&self, _f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            #[cfg(target_os = "macos")]
            SurfaceSource::Surface(ref buf) => _f.debug_tuple("Surface").field(buf).finish(),
            #[cfg(any(
                target_os = "linux",
                target_os = "freebsd",
                all(target_family = "wasm", feature = "custom-gpu")
            ))]
            SurfaceSource::Texture { size, .. } => _f
                .debug_struct("Texture")
                .field("size", &size)
                .finish_non_exhaustive(),
            #[cfg(target_os = "windows")]
            SurfaceSource::WindowsCapture(ref frame) => frame.fmt(_f),
            SurfaceSource::Unsupported(size) => _f.debug_tuple("Unsupported").field(&size).finish(),
        }
    }
}

impl SurfaceSource {
    #[doc(hidden)]
    pub fn size(&self) -> Size<DevicePixels> {
        match self {
            #[cfg(target_os = "macos")]
            SurfaceSource::Surface(buffer) => {
                gpui_types::size(buffer.get_width().into(), buffer.get_height().into())
            }
            #[cfg(any(
                target_os = "linux",
                target_os = "freebsd",
                all(target_family = "wasm", feature = "custom-gpu")
            ))]
            SurfaceSource::Texture { size, .. } => *size,
            #[cfg(target_os = "windows")]
            SurfaceSource::WindowsCapture(frame) => frame.size(),
            SurfaceSource::Unsupported(size) => *size,
        }
    }
}

#[cfg(target_os = "macos")]
impl From<CVPixelBuffer> for SurfaceSource {
    fn from(value: CVPixelBuffer) -> Self {
        SurfaceSource::Surface(value)
    }
}

#[cfg(target_os = "windows")]
impl From<WindowsScreenCaptureFrame> for SurfaceSource {
    fn from(value: WindowsScreenCaptureFrame) -> Self {
        SurfaceSource::WindowsCapture(value)
    }
}

#[cfg(target_os = "windows")]
use windows_061::Win32::Graphics::Direct3D11::ID3D11Texture2D;

/// A Windows Graphics Capture frame backed by its native D3D11 texture.
#[cfg(target_os = "windows")]
#[derive(Clone)]
pub struct WindowsScreenCaptureFrame {
    texture: Arc<ID3D11Texture2D>,
    size: Size<DevicePixels>,
    display_time: u64,
}

#[cfg(target_os = "windows")]
impl WindowsScreenCaptureFrame {
    #[doc(hidden)]
    #[cfg(feature = "screen-capture")]
    pub fn new(texture: ID3D11Texture2D, size: Size<DevicePixels>, display_time: u64) -> Self {
        Self {
            texture: Arc::new(texture),
            size,
            display_time,
        }
    }

    /// Returns the native texture containing this frame.
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// Returns the frame dimensions in device pixels.
    pub fn size(&self) -> Size<DevicePixels> {
        self.size
    }

    /// Returns the capture timestamp in 100-nanosecond units.
    pub fn display_time(&self) -> u64 {
        self.display_time
    }
}

#[cfg(target_os = "windows")]
impl std::fmt::Debug for WindowsScreenCaptureFrame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsScreenCaptureFrame")
            .field("size", &self.size)
            .field("display_time", &self.display_time)
            .finish_non_exhaustive()
    }
}
