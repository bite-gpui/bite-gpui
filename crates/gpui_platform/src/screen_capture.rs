use anyhow::Result;
use gpui_shared_string::SharedString;
use gpui_types::{DevicePixels, Size};

#[cfg(all(target_os = "windows", feature = "screen-capture"))]
#[allow(missing_docs)]
pub type PlatformScreenCaptureFrame = gpui_engine::WindowsScreenCaptureFrame;
#[cfg(not(feature = "screen-capture"))]
#[allow(missing_docs)]
pub type PlatformScreenCaptureFrame = ();
#[cfg(all(target_os = "macos", feature = "screen-capture"))]
#[allow(missing_docs)]
pub type PlatformScreenCaptureFrame = core_video::image_buffer::CVImageBuffer;
#[cfg(all(
    feature = "screen-capture",
    not(any(target_os = "macos", target_os = "windows"))
))]
// Screen capture currently has native frame representations only on macOS and Windows. Keep the
// cross-platform API well-formed for enabled-but-unsupported targets; source enumeration simply
// yields no platform sources there.
#[allow(missing_docs)]
pub type PlatformScreenCaptureFrame = ();

/// Metadata for a screen capture source.
#[derive(Clone)]
pub struct SourceMetadata {
    /// Opaque identifier of this screen.
    pub id: u64,
    /// Human-readable label for this source.
    pub label: Option<SharedString>,
    /// Whether this source is the main display.
    pub is_main: Option<bool>,
    /// Video resolution of this source.
    pub resolution: Size<DevicePixels>,
}

/// A video stream captured from a screen.
pub trait ScreenCaptureStream {
    /// Returns metadata for this source.
    fn metadata(&self) -> Result<SourceMetadata>;
}

/// A frame of video captured from a screen.
pub struct ScreenCaptureFrame(pub PlatformScreenCaptureFrame);
