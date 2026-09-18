//! Display abstractions shared by `gpui` and its platform backends.

use crate::DEFAULT_WINDOW_SIZE;
use anyhow::Result;
use gpui_types::{Bounds, Pixels, point};
use std::fmt::{self, Debug};
use std::time::Duration;
use uuid::Uuid;

/// An opaque identifier for a hardware display
#[derive(PartialEq, Eq, Hash, Copy, Clone)]
pub struct DisplayId(pub(crate) u64);

impl DisplayId {
    /// Create a new `DisplayId` from a raw platform display identifier.
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl From<u64> for DisplayId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<DisplayId> for u64 {
    fn from(id: DisplayId) -> Self {
        id.0
    }
}

impl Debug for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DisplayId({})", self.0)
    }
}

/// A change to the connected displays, reported through
/// `App::observe_displays`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DisplayEvent {
    /// A display was connected.
    Added(DisplayId),
    /// A display was disconnected. Windows on it are moved to another
    /// display by the platform.
    Removed(DisplayId),
    /// A connected display's properties changed.
    Changed {
        /// The display that changed.
        id: DisplayId,
        /// Which of its properties changed.
        changes: DisplayChanges,
    },
}

bitflags::bitflags! {
    /// The properties of a display that a [`DisplayEvent::Changed`] reports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct DisplayChanges: u8 {
        /// [`PlatformDisplay::bounds`] changed.
        const BOUNDS = 1 << 0;
        /// [`PlatformDisplay::refresh_interval`] changed.
        const REFRESH_INTERVAL = 1 << 1;
    }
}

/// Converts a refresh rate in hertz to the time between refreshes, rejecting
/// the zero, negative, and non-finite rates platforms use to mean "unknown".
pub fn refresh_interval_from_hz(hertz: f64) -> Option<Duration> {
    (hertz.is_finite() && hertz > 0.0).then(|| Duration::from_secs_f64(1.0 / hertz))
}

/// A handle to a platform's display, e.g. a monitor or laptop screen.
pub trait PlatformDisplay: Debug {
    /// Get the ID for this display
    fn id(&self) -> DisplayId;

    /// Returns a stable identifier for this display that can be persisted and used
    /// across system restarts.
    fn uuid(&self) -> Result<Uuid>;

    /// Get the bounds for this display
    fn bounds(&self) -> Bounds<Pixels>;

    /// The time between the display's refreshes. Variable refresh rate
    /// displays report their maximum rate. `None` when the platform doesn't
    /// report it.
    ///
    /// May query the operating system; GPUI caches it per display, see
    /// `Window::refresh_interval`.
    fn refresh_interval(&self) -> Option<Duration>;

    /// Get the visible bounds for this display, excluding taskbar/dock areas.
    /// This is the usable area where windows can be placed without being obscured.
    /// Defaults to the full display bounds if not overridden.
    fn visible_bounds(&self) -> Bounds<Pixels> {
        self.bounds()
    }

    /// Get the default bounds for this display to place a window
    fn default_bounds(&self) -> Bounds<Pixels> {
        let bounds = self.bounds();
        let center = bounds.center();
        let clipped_window_size = DEFAULT_WINDOW_SIZE.min(&bounds.size);

        let offset = clipped_window_size / 2.0;
        let origin = point(center.x - offset.width, center.y - offset.height);
        Bounds::new(origin, clipped_window_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_refresh_interval_from_hz() {
        assert_eq!(
            refresh_interval_from_hz(50.0),
            Some(Duration::from_millis(20))
        );
        assert_eq!(
            refresh_interval_from_hz(120.0),
            Some(Duration::from_secs(1) / 120)
        );
        for unknown in [0.0, -60.0, f64::NAN, f64::INFINITY] {
            assert_eq!(refresh_interval_from_hz(unknown), None);
        }
    }
}
