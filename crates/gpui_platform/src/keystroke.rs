//! The keystroke value types live in `gpui_types`; this module re-exports them so
//! `crate::platform::keystroke::Keystroke` and the paths through `platform::*`
//! keep resolving.

pub use gpui_types::{Capslock, KeybindingKeystroke, Keystroke, Modifiers};

use crate::keyboard::PlatformKeyboardMapper;

/// Compatibility shim for `KeybindingKeystroke::new_with_mapper`.
///
/// `KeybindingKeystroke` now lives in `gpui_types` and an inherent method has to
/// be defined in the crate that defines its type, so the method cannot stay on
/// the type: `PlatformKeyboardMapper` is a platform trait that sits *above* the
/// value crate. The mapper already returns the wrapper, so the direct call is
/// `keyboard_mapper.map_key_equivalent(keystroke, use_key_equivalents)`.
///
/// This trait exists only so that call sites written against the old inherent
/// method keep compiling, and it is deprecated in favour of the call above.
#[deprecated(note = "call `PlatformKeyboardMapper::map_key_equivalent` instead")]
pub trait KeybindingKeystrokeMapperExt {
    /// Map `inner` through the platform keyboard mapper.
    fn new_with_mapper(
        inner: Keystroke,
        use_key_equivalents: bool,
        keyboard_mapper: &dyn PlatformKeyboardMapper,
    ) -> Self;
}

#[allow(deprecated)]
impl KeybindingKeystrokeMapperExt for KeybindingKeystroke {
    fn new_with_mapper(
        inner: Keystroke,
        use_key_equivalents: bool,
        keyboard_mapper: &dyn PlatformKeyboardMapper,
    ) -> Self {
        keyboard_mapper.map_key_equivalent(inner, use_key_equivalents)
    }
}
