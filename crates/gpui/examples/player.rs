//! A video player: an H.264 stream hardware-decoded by VA-API, played into a window.
//!
//! ```sh
//! cargo run -p gpui --example player
//! cargo run -p gpui --example player -- path/to/stream.h264
//! ```
//!
//! Run with no argument it plays the bundled clip — sixty frames of SMPTE-style bars scrolling a
//! few pixels per frame, so the frames differ and the stream carries real inter-frame (`P`) frames.
//! Give it a path to play any H.264 elementary stream (Annex B) instead: the escaped `ffmpeg
//! -i in.mp4 -c:v copy -bsf:v h264_mp4toannexb -f h264 out.h264` turns a container into one.
//!
//! # What this is
//!
//! The player end of the escape hatch: `gpui_va` decodes the stream, each frame exports its VA-API
//! surface as a dma-buf under the DRM format modifier the driver chose, and the window's renderer
//! imports that buffer and converts it — no copy through the CPU, no copy on the GPU.
//!
//! # Bounded, not all-in-memory
//!
//! The playback keeps only a few frames alive and recycles each surface once the renderer says it is
//! done with it. Every handle the producer hands out carries a **release descriptor**; the renderer
//! signals it from the completion callback of the submission that sampled the frame, and the producer
//! waits on it before returning the surface to its pool — and never recycles the frame still on
//! screen, which the scene re-samples every frame. That is what lets a stream play in constant memory.
//!
//! # Colour
//!
//! The renderer converts NV12 with the matrix and range the *producer declares on the frame's
//! handle*, and `gpui_va` reads both off the stream — so footage plays as it was graded rather than
//! through one assumed matrix. The bundled clip is full-range BT.601. A stream that names neither (as
//! most do) is resolved by convention: limited range, and BT.601 below 576 lines, BT.709 above.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "player is a Linux example: it plays a stream through the VA-API surface path. Run it on \
         Linux."
    );
}

#[cfg(target_os = "linux")]
fn main() {
    demo::run();
}

#[cfg(target_os = "linux")]
mod demo {
    use std::time::{Duration, Instant};

    use gpui::{
        AnyElement, App, Bounds, Context, DmaBufHandle, IntoElement, Render, Window, WindowBounds,
        WindowOptions, div, prelude::*, px, rgb, size, surface,
    };
    use gpui_va::Playback;

    /// How many frames the playback keeps alive at once. Small, because recycling is what allows it.
    const DEPTH: usize = 4;

    /// The bundled clip's rate, 25 fps.
    fn frame_period() -> Duration {
        Duration::from_millis(40)
    }

    pub fn run() {
        gpui::application().run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(640.), px(400.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Player::new()),
            )
            .expect("a window to play into");
            cx.activate(true);
        });
    }

    /// The stream to play: a file named on the command line, or the bundled clip.
    fn stream() -> Option<(String, Vec<u8>)> {
        match std::env::args().nth(1) {
            Some(path) => std::fs::read(&path).ok().map(|bytes| (path, bytes)),
            None => Some(("bundled clip".to_owned(), gpui_va::CLIP.to_vec())),
        }
    }

    struct Player {
        playback: Option<Playback>,
        /// The frame on screen: the scene re-samples it until the playback advances.
        current: Option<DmaBufHandle>,
        /// Where the frames came from, or why there are none.
        source: String,
        playing: bool,
        /// When the current frame came due, so playback keeps the stream's rate.
        due: Instant,
    }

    impl Player {
        fn new() -> Self {
            let (playback, source) = match stream() {
                Some((source, bytes)) => match Playback::open(&bytes, DEPTH) {
                    Some(playback) => (Some(playback), source),
                    None => (
                        None,
                        "no stream: libavcodec of the declared ABI, or the VA-API device, is missing"
                            .to_owned(),
                    ),
                },
                None => (None, "cannot read the named stream".to_owned()),
            };
            let mut player = Self {
                playback,
                current: None,
                source,
                playing: true,
                due: Instant::now(),
            };
            player.advance();
            player
        }

        /// Take the next frame to show, if the playback has one to give.
        fn advance(&mut self) {
            if let Some(playback) = &mut self.playback
                && let Some(handle) = playback.next()
            {
                self.current = Some(handle);
            }
        }
    }

    impl Render for Player {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.playing && self.playback.is_some() {
                // Advance every frame the wall clock says is due, so playback keeps the stream's rate
                // however fast the window redraws. A frame the renderer has not released yet is
                // simply shown again.
                let now = Instant::now();
                while now.duration_since(self.due) >= frame_period() {
                    self.due += frame_period();
                    self.advance();
                }
                window.request_animation_frame();
            }

            let picture: AnyElement = match &self.current {
                Some(handle) => surface(handle.clone()).size_full().into_any_element(),
                None => div().size_full().bg(rgb(0x202024)).into_any_element(),
            };

            let status = match &self.playback {
                Some(playback) => format!(
                    "{} · frame {} · {} live (of {DEPTH}) · {}",
                    self.source,
                    playback.position(),
                    playback.live(),
                    if self.playing { "playing" } else { "paused" },
                ),
                None => self.source.clone(),
            };

            div()
                .size_full()
                .bg(rgb(0x101014))
                .flex()
                .flex_col()
                .gap_2()
                .p_4()
                .child(
                    div()
                        .id("player-picture")
                        .flex_1()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.playing = !this.playing;
                            if this.playing {
                                this.due = Instant::now();
                            }
                            cx.notify();
                        }))
                        .child(picture),
                )
                .child(div().text_sm().text_color(rgb(0x9a9aa2)).child(status))
        }
    }
}
