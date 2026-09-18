//! The [`Platform`] trait implemented by each platform backend.
//!
//! `gpui` drives an application through a `dyn Platform`; backends
//! (`gpui_macos`, `gpui_linux`, `gpui_windows`, `gpui_web`) implement it
//! without depending on the framework itself.

use crate::{
    ActivityGuard, AppLifecyclePhase, BackgroundExecutor, ClipboardItem, ClipboardReadError,
    CursorStyle, ForegroundExecutor, MenuCommandId, PathPromptOptions, PlatformDisplay,
    PlatformGestures, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformMenu,
    PlatformMenuItem, PlatformWindow, ScreenCaptureSource, SystemNotification,
    SystemNotificationResponse, Task, ThermalState, WindowAppearance, WindowButtonLayout, WindowId,
    WindowParams,
};
use anyhow::Result;
use futures::channel::oneshot;
use gpui_backend::PlatformTextSystem;
use smallvec::SmallVec;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

// TODO(jk): return an enum instead of a string
/// Return which compositor we're guessing we'll use.
/// Does not attempt to connect to the given compositor.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[inline]
pub fn guess_compositor() -> &'static str {
    if std::env::var_os("ZED_HEADLESS").is_some() {
        return "Headless";
    }
    GraphicalEnvironment::detect().guess_compositor()
}

/// The graphical session to connect to: the variables that locate its display server, as some
/// process sees them.
///
/// A long-running process can outlive the graphical session it was started in, so a platform
/// that attaches to a display server later can be given a fresher environment than its own.
/// While connected, programs the platform launches (for example to open a URL) get these
/// variables instead of the ones this process started with. Apply
/// [`App::graphical_environment`] to the programs an app launches, for the same reason.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[derive(Clone, Debug, Default)]
pub struct GraphicalEnvironment {
    /// `WAYLAND_DISPLAY`: a socket name relative to `xdg_runtime_dir`, or an absolute path.
    pub wayland_display: Option<OsString>,
    /// `DISPLAY`: the X11 display name.
    pub x11_display: Option<OsString>,
    /// `XDG_RUNTIME_DIR`: the directory containing Wayland sockets.
    pub xdg_runtime_dir: Option<OsString>,
    /// `XDG_ACTIVATION_TOKEN`: lets the first window take focus on Wayland.
    ///
    /// A platform takes this process's token from its environment when it's created, and uses
    /// it if it starts windowed. Pass one here to switch to windowed mode later, for example
    /// the token of the process that asked for a window.
    pub activation_token: Option<String>,
}

/// The graphical session to connect to: the Windows session whose desktop windows appear on.
///
/// A process can only show windows in its own session, so switching to windowed mode fails if
/// this names another one, for example when a process started over SSH is asked to show a
/// window on the desktop.
#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Default)]
pub struct GraphicalEnvironment {
    /// The session ID, as `ProcessIdToSessionId` reports it. `None` means this process's own
    /// session.
    pub session_id: Option<u32>,
}

#[cfg(target_os = "windows")]
impl GraphicalEnvironment {
    /// Returns the environment of this process's session.
    pub fn detect() -> Self {
        let mut session_id = 0;
        // SAFETY: `session_id` is a valid pointer for the call's duration.
        let result = unsafe {
            windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                windows::Win32::System::Threading::GetCurrentProcessId(),
                &mut session_id,
            )
        };
        Self {
            session_id: result.is_ok().then_some(session_id),
        }
    }

    /// Does nothing on this platform: programs inherit the session of the process that
    /// starts them.
    pub fn apply_to(&self, _command: &mut std::process::Command) {}
}

/// The graphical session to connect to. Carries nothing yet on this platform.
#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
#[derive(Clone, Debug, Default)]
pub struct GraphicalEnvironment;

#[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "windows")))]
impl GraphicalEnvironment {
    /// Returns the environment of this process's graphical session.
    pub fn detect() -> Self {
        Self
    }

    /// Does nothing on this platform.
    pub fn apply_to(&self, _command: &mut std::process::Command) {}
}

/// A display mode for [`App::request_windowing`] to switch to.
#[derive(Clone, Debug)]
pub enum WindowingRequest {
    /// No display server. Windows lay out and handle input but draw nothing.
    Headless,
    /// Connected to the display server that the environment names.
    Windowed(GraphicalEnvironment),
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl GraphicalEnvironment {
    /// Reads the display variables from this process's environment.
    ///
    /// This only reads environment variables: whether they name a reachable display server is
    /// checked when a platform connects. Leaves `activation_token` unset: see its documentation.
    pub fn detect() -> Self {
        Self {
            wayland_display: std::env::var_os("WAYLAND_DISPLAY"),
            x11_display: std::env::var_os("DISPLAY"),
            xdg_runtime_dir: std::env::var_os("XDG_RUNTIME_DIR"),
            activation_token: None,
        }
    }

    /// Sets this environment's display variables on `command`, and removes the ones it doesn't
    /// set, so the program connects to this graphical session rather than the one this process
    /// started in. Leaves `XDG_ACTIVATION_TOKEN` alone.
    pub fn apply_to(&self, command: &mut std::process::Command) {
        for (name, value) in [
            ("WAYLAND_DISPLAY", &self.wayland_display),
            ("DISPLAY", &self.x11_display),
            ("XDG_RUNTIME_DIR", &self.xdg_runtime_dir),
        ] {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
    }

    /// Returns the compositor this environment selects: Wayland, then X11, then headless.
    ///
    /// Does not attempt to connect to the compositor.
    pub fn guess_compositor(&self) -> &'static str {
        let is_set =
            |value: &Option<OsString>| value.as_ref().is_some_and(|value| !value.is_empty());
        if cfg!(feature = "wayland") && is_set(&self.wayland_display) {
            "Wayland"
        } else if cfg!(feature = "x11") && is_set(&self.x11_display) {
            "X11"
        } else {
            "Headless"
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
bitflags::bitflags! {
    /// The windowing modes a platform may start in or switch to.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct WindowingModes: u8 {
        /// Connected to a Wayland compositor.
        const WAYLAND = 1 << 0;
        /// Connected to an X server.
        const X11 = 1 << 1;
        /// No display server. Windows lay out and handle input but draw nothing.
        const HEADLESS = 1 << 2;
    }
}

/// Controls whether the application participates in the system's foreground UI.
///
/// Only has an effect on macOS; other platforms ignore this setting. There,
/// [`App::request_windowing`] sets it: `Accessory` while headless and `Regular` while windowed.
/// Set it directly with [`App::set_activation_policy`] for the one case that doesn't cover: an
/// accessory app that shows windows, such as a menu bar utility.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ActivationPolicy {
    /// Participate in foreground application UI, such as the Dock and menu bar on macOS.
    #[default]
    Regular,
    /// Run without foreground application UI while retaining the ability to open windows.
    Accessory,
}

#[expect(missing_docs)]
pub trait Platform: 'static {
    fn background_executor(&self) -> BackgroundExecutor;
    fn foreground_executor(&self) -> ForegroundExecutor;
    fn text_system(&self) -> Arc<dyn PlatformTextSystem>;

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>);
    fn quit(&self);
    /// Switches a capable platform between headless and windowed modes. See
    /// [`App::request_windowing`].
    /// Sets the windowing mode the platform starts in. Called before `run`. See
    /// [`Application::with_windowing`].
    fn set_initial_windowing(&self, _request: WindowingRequest) {}
    fn request_windowing(&self, _request: WindowingRequest) -> Task<anyhow::Result<()>> {
        Task::ready(Err(anyhow::anyhow!(
            "this platform cannot switch between headless and windowed modes"
        )))
    }
    fn restart(&self, binary_path: Option<PathBuf>, arguments: Vec<OsString>);
    fn activate(&self, ignoring_other_apps: bool);
    /// Sets the initial or current activation policy. Has no effect outside macOS.
    fn set_activation_policy(&self, _policy: ActivationPolicy) {}
    fn hide(&self);
    fn hide_other_apps(&self);
    fn unhide_other_apps(&self);

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>>;
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>>;
    fn active_window(&self) -> Option<WindowId>;
    fn window_stack(&self) -> Option<Vec<WindowId>> {
        None
    }

    fn is_screen_capture_supported(&self) -> bool {
        false
    }

    fn screen_capture_sources(
        &self,
    ) -> oneshot::Receiver<anyhow::Result<Vec<Rc<dyn ScreenCaptureSource>>>> {
        let (sources_tx, sources_rx) = oneshot::channel();
        sources_tx
            .send(Err(anyhow::anyhow!(
                "gpui was compiled without the screen-capture feature"
            )))
            .ok();
        sources_rx
    }

    fn open_window(
        &self,
        handle: WindowId,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>>;

    /// Returns the appearance of the application's windows.
    fn window_appearance(&self) -> WindowAppearance;

    /// Overrides the appearance (light/dark) applied to the app's windows, independent
    /// of the OS-wide setting. Pass `None` to clear the override and follow the system
    /// again. The override is reflected by [`Platform::window_appearance`].
    ///
    /// Currently only implemented on macOS, where it sets `NSApplication.appearance` so
    /// the native window chrome (the window border and titlebar) of every window matches
    /// a dark app theme even when the system is in light mode (or vice versa). A no-op on
    /// other platforms.
    fn set_window_appearance(&self, _appearance: Option<WindowAppearance>) {}

    /// Returns the window button layout configuration when supported.
    fn button_layout(&self) -> Option<WindowButtonLayout> {
        None
    }

    fn open_url(&self, url: &str);
    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>);
    fn register_url_scheme(&self, url: &str) -> Task<Result<()>>;

    fn prompt_for_paths(
        &self,
        options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>>;
    fn prompt_for_new_path(
        &self,
        directory: &Path,
        suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>>;
    fn can_select_mixed_files_and_dirs(&self) -> bool;
    fn reveal_path(&self, path: &Path);
    fn open_with_system(&self, path: &Path);

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>);
    fn on_reopen(&self, callback: Box<dyn FnMut()>);
    /// Registers the callback invoked when the system is about to sleep.
    fn on_system_sleep(&self, callback: Box<dyn FnMut()>);
    /// Registers the callback invoked when the system resumes from sleep.
    fn on_system_wake(&self, callback: Box<dyn FnMut()>);

    // Mobile platform methods. On mobile the OS owns the application
    // lifecycle: apps are backgrounded, foregrounded, and killed at the
    // system's discretion, and must react rather than decide.

    /// Registers a callback invoked whenever the application's lifecycle
    /// phase changes. See [`AppLifecyclePhase`] for the phase vocabulary and
    /// its mapping onto iOS and Android.
    ///
    /// Desktop platforms never invoke this.
    fn on_app_lifecycle(&self, _callback: Box<dyn FnMut(AppLifecyclePhase)>) {}

    /// Registers a callback invoked when the OS signals memory pressure
    /// (iOS `didReceiveMemoryWarning`, Android `onTrimMemory`).
    ///
    /// Desktop platforms never invoke this.
    fn on_memory_warning(&self, _callback: Box<dyn FnMut()>) {}

    /// The platform's gesture recognition services, if it provides any
    /// beyond gpui's portable recognizers. See
    /// [`PlatformGestures`](crate::PlatformGestures).
    fn gestures(&self) -> Option<Rc<dyn PlatformGestures>> {
        None
    }

    fn set_menus(&self, menus: Vec<PlatformMenu>);

    fn set_dock_menu(&self, menu: Vec<PlatformMenuItem>);
    fn perform_dock_menu_action(&self, _action: usize) {}
    fn add_recent_document(&self, _path: &Path) {}
    fn update_jump_list(
        &self,
        _menus: Vec<PlatformMenuItem>,
        _entries: Vec<SmallVec<[PathBuf; 2]>>,
    ) -> Task<Vec<SmallVec<[PathBuf; 2]>>> {
        Task::ready(Vec::new())
    }
    fn on_app_menu_action(&self, callback: Box<dyn FnMut(MenuCommandId)>);
    fn on_will_open_app_menu(&self, callback: Box<dyn FnMut()>);
    fn on_validate_app_menu_command(&self, callback: Box<dyn FnMut(MenuCommandId) -> bool>);

    fn thermal_state(&self) -> ThermalState;
    fn on_thermal_state_change(&self, callback: Box<dyn FnMut()>);
    fn prevent_idle_sleep(&self, reason: &str) -> Task<Result<ActivityGuard>>;

    /// Sets the application's process-wide identity and user-visible name.
    ///
    /// The identifier is used for platform identity mechanisms such as the
    /// Windows AppUserModelID. The name is used wherever the operating system
    /// presents the application to the user. Call this once, early in startup,
    /// before opening windows or posting notifications.
    fn set_app_identity(&self, identifier: &str, name: &str) {
        _ = (identifier, name);
    }

    /// Posts a notification to the operating system's notification center.
    ///
    /// Posting a notification whose [`SystemNotification::tag`] matches an
    /// earlier one replaces that notification where the platform supports it.
    /// No-op on platforms without notification support, or when delivery is
    /// unavailable (e.g. authorization was denied).
    fn show_system_notification(&self, notification: SystemNotification) {
        _ = notification;
    }

    /// Removes the delivered or pending notification with this tag.
    ///
    /// Best-effort: some platforms cannot retract a notification once shown,
    /// in which case it ages out of the notification center on its own.
    fn dismiss_system_notification(&self, tag: &str) {
        _ = tag;
    }

    /// Registers the callback invoked when the user activates a system
    /// notification, either by clicking its body or one of its action
    /// buttons.
    ///
    /// Implementations must invoke the callback on the main thread.
    fn on_system_notification_response(
        &self,
        callback: Box<dyn FnMut(SystemNotificationResponse)>,
    ) {
        _ = callback;
    }

    fn compositor_name(&self) -> &'static str {
        ""
    }
    /// The environment of the display server a platform that can switch windowing modes is
    /// connected to. See [`App::graphical_environment`].
    fn graphical_environment(&self) -> Option<GraphicalEnvironment> {
        None
    }
    fn app_path(&self) -> Result<PathBuf>;
    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf>;

    fn set_cursor_style(&self, style: CursorStyle);

    /// Hides the mouse cursor until the user moves the mouse over one of
    /// this application's windows.
    fn hide_cursor_until_mouse_moves(&self);

    /// Returns whether the mouse cursor is currently visible.
    fn is_cursor_visible(&self) -> bool;

    fn should_auto_hide_scrollbars(&self) -> bool;

    fn read_from_clipboard(&self) -> Option<ClipboardItem>;
    fn write_to_clipboard(&self, item: ClipboardItem);

    /// Reads the clipboard, resolving once its contents are available.
    ///
    /// Most platforms read synchronously and return a ready task. Platforms
    /// whose clipboard access is inherently asynchronous and permission-gated
    /// (e.g. the browser's async clipboard API) override this method; on those
    /// platforms [`Platform::read_from_clipboard`] cannot return the clipboard
    /// contents, so callers that can await should prefer this method.
    fn read_from_clipboard_async(&self) -> Task<Result<Option<ClipboardItem>, ClipboardReadError>> {
        Task::ready(Ok(self.read_from_clipboard()))
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn read_from_primary(&self) -> Option<ClipboardItem>;
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fn write_to_primary(&self, item: ClipboardItem);

    #[cfg(target_os = "macos")]
    fn read_from_find_pasteboard(&self) -> Option<ClipboardItem>;
    #[cfg(target_os = "macos")]
    fn write_to_find_pasteboard(&self, item: ClipboardItem);

    fn write_credentials(&self, url: &str, username: &str, password: &[u8]) -> Task<Result<()>>;
    fn read_credentials(&self, url: &str) -> Task<Result<Option<(String, Vec<u8>)>>>;
    fn delete_credentials(&self, url: &str) -> Task<Result<()>>;

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout>;
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper>;
    fn on_keyboard_layout_change(&self, callback: Box<dyn FnMut()>);
}
