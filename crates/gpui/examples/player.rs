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
//! # The one thing to know
//!
//! A VA surface is recycled by the decoder's pool when its `AVFrame` is released, so a frame still
//! being sampled must not be released. This player therefore holds **every** decoded frame and
//! cycles an index over them: playback is bounded by the stream's size in memory, not by a window of
//! a few frames, which is what keeps it simple *and* free of the recycle-under-the-renderer tear a
//! bounded queue has to fence against.
//!
//! # Colour
//!
//! The renderer's surface shader converts NV12 with one fixed matrix, BT.601 full-range. The bundled
//! clip is encoded to match, so it plays as drawn; a stream tagged otherwise (a limited-range
//! BT.709 stream, as most real footage is) will look washed out or hue-shifted until the shader is
//! told the stream's colour space.

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
        AnyElement, App, Bounds, Context, IntoElement, Render, Window, WindowBounds, WindowOptions,
        div, prelude::*, px, rgb, size, surface,
    };

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
            Some(path) => std::fs::read(&path)
                .ok()
                .map(|bytes| (path, bytes)),
            None => Some(("bundled clip".to_owned(), gpui_va::CLIP.to_vec())),
        }
    }

    /// Decode a whole stream into the frames a player cycles through.
    ///
    /// Holding every frame is deliberate; see the module docs.
    fn decode(stream: &[u8]) -> Option<Vec<gpui_va::Frame>> {
        let mut decoder = gpui_va::Decoder::open()?;
        decoder.send(stream).ok()?;
        decoder.finish().ok()?;
        let mut frames = Vec::new();
        while let Some(frame) = decoder.receive() {
            frames.push(frame);
        }
        (!frames.is_empty()).then_some(frames)
    }

    struct Player {
        /// Where the frames came from, for the caption.
        source: String,
        /// Every decoded frame, held so no surface is recycled under the renderer.
        frames: Vec<gpui_va::Frame>,
        index: usize,
        playing: bool,
        /// When the current frame came due, so playback keeps the stream's rate.
        due: Instant,
    }

    impl Player {
        fn new() -> Self {
            let (source, frames) = match stream()
                .and_then(|(source, bytes)| decode(&bytes).map(|frames| (source, frames)))
            {
                Some((source, frames)) => (source, frames),
                None => (
                    "no stream: libavcodec of the declared ABI, or the VA-API device, is missing"
                        .to_owned(),
                    Vec::new(),
                ),
            };
            Self {
                source,
                frames,
                index: 0,
                playing: true,
                due: Instant::now(),
            }
        }

        fn advance(&mut self) {
            if !self.frames.is_empty() {
                self.index = (self.index + 1) % self.frames.len();
            }
        }
    }

    impl Render for Player {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            if self.playing && !self.frames.is_empty() {
                // Advance every frame the wall clock says is due, so playback keeps the stream's
                // rate however fast the window redraws.
                let now = Instant::now();
                while now.duration_since(self.due) >= frame_period() {
                    self.due += frame_period();
                    self.advance();
                }
                window.request_animation_frame();
            }

            let picture: AnyElement = match self.frames.get(self.index) {
                Some(frame) => surface(frame.handle().clone())
                    .size_full()
                    .into_any_element(),
                None => div().size_full().bg(rgb(0x202024)).into_any_element(),
            };

            let status = format!(
                "{} · frame {} / {} · {}",
                self.source,
                if self.frames.is_empty() {
                    0
                } else {
                    self.index + 1
                },
                self.frames.len(),
                if self.playing { "playing" } else { "paused" },
            );

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
