// The parent-anchored popup vocabulary moved down into `gpui_platform`; re-exported so the
// `crate::popup::…` paths keep resolving.
pub use gpui_platform::{
    PopupAnchor, PopupConstraintAdjustment, PopupGravity, PopupNotSupportedError, PopupOptions,
};
