use bitflags::bitflags;

use crate::{AnyWindowHandle, Bounds, Pixels, Point};

// The anchor/gravity descriptors and the unsupported-platform error moved down into
// `gpui_platform`; re-exported so the `crate::popup::…` paths keep resolving.
pub use gpui_platform::{PopupAnchor, PopupGravity, PopupNotSupportedError};

/// Options for a parent-anchored popup window such as a menu, dropdown, context menu or tooltip.
///
/// A popup is placed relative to an anchor rectangle on its parent window rather than at an
/// absolute screen position. The platform resolves the final position, so this works both on
/// systems where the compositor owns window placement (Wayland) and on platforms with absolute
/// coordinates.
///
/// The popup's size comes from [`WindowOptions::window_bounds`](crate::WindowOptions), whose
/// origin is ignored. All coordinates are in logical pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PopupOptions {
    /// The window the popup is anchored to.
    pub parent: AnyWindowHandle,

    /// The rectangle the popup is positioned relative to, in the parent window's coordinate
    /// space (the same space element bounds are in). For example, a dropdown menu uses the
    /// bounds of the button that opened it.
    pub anchor_rect: Bounds<Pixels>,

    /// Which point of [`Self::anchor_rect`] the popup is anchored to.
    pub anchor: PopupAnchor,

    /// The direction in which the popup extends away from the anchor point. A dropdown that
    /// drops below its button anchors to [`PopupAnchor::BottomLeft`] with a gravity of
    /// [`PopupGravity::BottomRight`] so it grows down and to the right.
    pub gravity: PopupGravity,

    /// How the platform may adjust the popup if the requested placement would put it off-screen.
    pub constraint_adjustment: PopupConstraintAdjustment,

    /// An additional offset applied to the popup after anchoring.
    pub offset: Point<Pixels>,

    /// Whether the popup should take an explicit input grab.
    ///
    /// Grabbing popups behave like menus: they take keyboard focus and are dismissed when the
    /// user clicks outside of them or presses a dismissing key. Use it for menus and comboboxes,
    /// not for tooltips or other passive popups.
    ///
    /// A grab must be requested while the triggering input is still active, in practice the
    /// press of the mouse button that opens the popup. Open grabbing popups from a mouse-down
    /// handler rather than a click handler, otherwise the grab is refused.
    ///
    /// Automatic dismissal only covers input aimed at other applications. A click elsewhere in
    /// your own application still reaches it as usual, so closing the popup in that case is up
    /// to you. Nested grabbing popups must be closed in the reverse order they were opened.
    pub grab: bool,
}

bitflags! {
    /// How a popup may be adjusted by the platform if the requested placement would put it
    /// off-screen. If no flags are set, the popup is placed exactly as requested and may be
    /// clipped.
    #[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
    pub struct PopupConstraintAdjustment: u32 {
        /// The popup may be slid horizontally to stay on-screen.
        const SLIDE_X = 1;
        /// The popup may be slid vertically to stay on-screen.
        const SLIDE_Y = 2;
        /// The popup's anchor and gravity may be flipped horizontally to stay on-screen.
        const FLIP_X = 4;
        /// The popup's anchor and gravity may be flipped vertically to stay on-screen.
        const FLIP_Y = 8;
        /// The popup may be shrunk horizontally to stay on-screen.
        const RESIZE_X = 16;
        /// The popup may be shrunk vertically to stay on-screen.
        const RESIZE_Y = 32;
    }
}
