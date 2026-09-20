// TODO(jk): return an enum instead of a string
/// Return which compositor we're guessing we'll use.
/// Does not attempt to connect to the given compositor.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub fn guess_compositor() -> &'static str {
    if std::env::var_os("ZED_HEADLESS").is_some() {
        return "Headless";
    }

    #[cfg(feature = "wayland")]
    let wayland_display = std::env::var_os("WAYLAND_DISPLAY");
    #[cfg(not(feature = "wayland"))]
    let wayland_display: Option<std::ffi::OsString> = None;

    #[cfg(feature = "x11")]
    let x11_display = std::env::var_os("DISPLAY");
    #[cfg(not(feature = "x11"))]
    let x11_display: Option<std::ffi::OsString> = None;

    let use_wayland = wayland_display.is_some_and(|display| !display.is_empty());
    let use_x11 = x11_display.is_some_and(|display| !display.is_empty());

    if use_wayland {
        "Wayland"
    } else if use_x11 {
        "X11"
    } else {
        "Headless"
    }
}

/// The activation policy for a macOS application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MacActivationPolicy {
    /// The application is an ordinary app that appears in the Dock and may have a user interface.
    #[default]
    Regular,
    /// The application doesn't appear in the Dock and doesn't have a menu bar, but it may be activated programmatically or by clicking on one of its windows.
    Accessory,
    /// The application doesn't appear in the Dock and may not create windows or be activated.
    Prohibited,
}

/// Styles of haptic feedback that can be played via the platform.
///
/// These correspond directly to [`NSHapticFeedbackPattern`](https://developer.apple.com/documentation/appkit/nshapticfeedbackmanager/feedbackpattern)
/// values on macOS. On other platforms, all styles are no-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HapticFeedbackStyle {
    /// A generic haptic tap — suitable for most interactions.
    Generic,
    /// A sharp snap — for alignment guides, detents, and snapping.
    Alignment,
    /// A distinct level-change click — for slider steps, toggles, and
    /// discrete state changes.
    LevelChange,
}
