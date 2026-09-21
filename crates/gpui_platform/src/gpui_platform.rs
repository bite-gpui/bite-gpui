//! Abstract platform traits and value types shared by `gpui` and its platform backends.
//!
//! This crate sits between the value crates (`gpui_types`, `gpui_engine`) and the `gpui`
//! facade: it holds the parts of the platform SPI that carry no runtime vocabulary of
//! `gpui` itself — window/display/appearance descriptors, the keyboard and display traits,
//! notifications, prompts, cursor and clipboard strings, and the screen-capture value
//! types. A backend can implement these without depending on all of `gpui`.
//!
//! `gpui` re-exports everything here, so `gpui::CursorStyle` and
//! `gpui_platform::CursorStyle` name the same type.
//!
//! ## Layout
//!
//! Module names follow the layered reference layout (`app`, `clipboard`, `cursor`,
//! `display`, `executor`, `input_handler`, `keyboard`, `layer_shell`, `menu`,
//! `notification`, `platform`, `popup`, `profiler`, `prompt`, `screen_capture`,
//! `test_dispatcher`, `text_input`, `window`). Two of them are transitional groupings
//! rather than reference
//! modules: the test doubles taken from ce's `platform/test/` (`TestAtlas`,
//! `TestAtlasState`, `TestDisplay`, `TestKeyboardLayout`, `TestPrompt`, `TestPrompts`,
//! `TestSystemNotifications`) are folded into the module of their subject, and
//! `platform.rs` holds the leftovers of ce's `crates/gpui/src/platform.rs`. `layer_shell`
//! is exposed as a module rather than flattened, because its `Anchor` would collide with
//! `gpui_types::Anchor` in the facade's glob imports.

#![warn(missing_docs)]
// Mirrors `gpui`: the moved code carries a `mut` binding that only some platform cfgs make
// redundant.
#![allow(unused_mut)]

mod app;
mod clipboard;
mod cursor;
mod display;
mod executor;
mod input_handler;
mod keyboard;
mod keystroke;
#[cfg(all(target_os = "linux", feature = "wayland"))]
#[expect(missing_docs)]
pub mod layer_shell;
mod menu;
mod notification;
mod platform;
mod popup;
mod profiler;
mod prompt;
#[cfg(any(
    test,
    target_os = "windows",
    target_os = "linux",
    target_family = "wasm",
    feature = "test-support",
    feature = "bench-support"
))]
#[expect(missing_docs)]
pub mod queue;
mod screen_capture;
mod text_input;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
mod test_dispatcher;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
mod threaded_dispatcher;
mod window;

pub use app::*;
pub use clipboard::*;
pub use cursor::*;
pub use display::*;
pub use executor::*;
pub use input_handler::*;
pub use keyboard::*;
pub use keystroke::*;
pub use menu::*;
pub use notification::*;
pub use platform::*;
pub use popup::*;
pub use profiler::*;
pub use prompt::*;
pub use screen_capture::*;
pub use text_input::*;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
pub use test_dispatcher::*;
#[cfg(any(test, feature = "test-support", feature = "bench-support"))]
pub use threaded_dispatcher::*;
pub use window::*;

pub use gpui_shared_string::*;
pub use gpui_types::*;
