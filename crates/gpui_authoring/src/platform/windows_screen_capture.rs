//! `WindowsScreenCaptureFrame` moved down into `gpui_engine`, where the `SurfaceSource`
//! that carries it lives. Re-exported here so the `platform::WindowsScreenCaptureFrame`
//! path and the crate-root re-export keep resolving.

pub use gpui_engine::WindowsScreenCaptureFrame;
