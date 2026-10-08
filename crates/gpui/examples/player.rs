//! A video player: a stream hardware-decoded by VA-API, played into a window.
//!
//! ```sh
//! cargo run -p gpui --example player
//! cargo run -p gpui --example player -- path/to/movie.mp4
//! ```
//!
//! Run with no argument it plays the bundled clip — sixty frames of SMPTE-style bars scrolling a
//! few pixels per frame, so the frames differ and the stream carries real inter-frame (`P`) frames.
//! Give it a path to play any stream libavformat opens: a container such as MP4 or Matroska, or a raw
//! H.264 elementary stream (Annex B). A container's packets are framed and its parameter sets kept
//! out of band, so it is decoded through the demuxer rather than the bitstream parser.
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
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use gpui::{
        AnyElement, App, Bounds, Context, DmaBufHandle, FramePipelineExt, IntoElement, PhaseMetrics,
        Render, StandardImmediatePipeline, Window, WindowBounds, WindowOptions, div, prelude::*, px,
        rgb, size, surface,
    };
    use gpui_va::Playback;

    /// How many frames the playback keeps alive at once. Small, because recycling is what allows it.
    const DEPTH: usize = 4;

    /// The bundled clip's rate, 25 fps, for a stream that declares no rate of its own.
    const BUNDLED_PERIOD: Duration = Duration::from_millis(40);

    /// The window redraws at the display's rate, but the whole tree is laid out afresh every frame —
    /// so drawing at 60 Hz for a 25 fps stream pays for a layout the picture did not change. Capping
    /// the draws near the stream's rate makes the skipped frames cost nothing.
    const MAX_FPS: u32 = 30;

    pub fn run() {
        // Time the frame's passes, so a report can say where a frame's work goes.
        let metrics = Rc::new(RefCell::new(PhaseMetrics::default()));
        gpui::application()
            .with_frame_pipeline({
                let metrics = metrics.clone();
                move |_window_id| {
                    Box::new(
                        StandardImmediatePipeline
                            .max_fps(MAX_FPS)
                            .instrumented(metrics.clone()),
                    )
                }
            })
            .run(move |cx: &mut App| {
                let bounds = Bounds::centered(None, size(px(640.), px(400.)), cx);
                cx.open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        ..Default::default()
                    },
                    move |_, cx| cx.new(|_| Player::new(metrics.clone())),
                )
                .expect("a window to play into");
                cx.activate(true);
            });
    }

    /// The player: the bounded playback over the chosen stream, and the frame on screen.
    struct Player {
        playback: Option<Playback>,
        /// The frame on screen: the scene re-samples it until the playback advances.
        current: Option<DmaBufHandle>,
        /// Where the frames came from, or why there are none.
        source: String,
        playing: bool,
        /// When the current frame came due, so playback keeps the stream's rate.
        due: Instant,
        /// The frame pipeline's per-pass timings, reported periodically while tracing.
        metrics: Rc<RefCell<PhaseMetrics>>,
        /// Time spent in `advance` (decoding and exporting a frame) and how many calls made it.
        advance_time: Duration,
        advance_calls: u64,
        /// The frame count the last report covered, so each report prints once.
        reported_frames: usize,
    }

    impl Player {
        fn new(metrics: Rc<RefCell<PhaseMetrics>>) -> Self {
            let (playback, source) = match std::env::args().nth(1) {
                // A named file goes through libavformat, so a container (MP4, Matroska, …) plays as
                // readily as a raw elementary stream.
                Some(path) => match Playback::open_path(Path::new(&path), DEPTH) {
                    Some(playback) => (Some(playback), path),
                    None => (
                        None,
                        "no stream: libavformat/libavcodec of the declared ABI, or the VA-API \
                         device, is missing"
                            .to_owned(),
                    ),
                },
                None => match Playback::open(gpui_va::CLIP, DEPTH) {
                    Some(playback) => (Some(playback), "bundled clip".to_owned()),
                    None => (
                        None,
                        "no stream: libavcodec of the declared ABI, or the VA-API device, is missing"
                            .to_owned(),
                    ),
                },
            };
            let mut player = Self {
                playback,
                current: None,
                source,
                playing: true,
                due: Instant::now(),
                metrics,
                advance_time: Duration::ZERO,
                advance_calls: 0,
                reported_frames: 0,
            };
            player.advance();
            player
        }

        /// How long each frame is due for: the stream's own rate where it declares one, else the
        /// bundled clip's.
        fn frame_period(&self) -> Duration {
            self.playback
                .as_ref()
                .and_then(Playback::frame_period)
                .unwrap_or(BUNDLED_PERIOD)
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
            // Take the frame this render will show, and at most one per render. The frame is taken
            // *before* the picture is built, so the render that takes it is the render that draws
            // it. A frame taken and left for the next render is orphaned if that render never comes
            // — the window losing focus mid-frame, or a draw the pipeline defers — and an orphaned
            // frame is never composited, so the renderer never signals its release and the bounded
            // playback waits on it for good. The wall clock keeps the stream's rate, and a run-ahead
            // — the window stalled — is resynced rather than fast-forwarded through frames that would
            // then never be shown.
            //
            // Only a frame the window can show is taken, and only a shown window keeps the loop
            // demanding: a frame drawn for a hidden window is never presented, so it is never
            // released either. While the window is hidden the loop parks, and the platform requests
            // a frame once it is shown, resuming playback at the current frame rather than
            // fast-forwarding through the hidden stretch.
            if self.playing && self.playback.is_some() && window.is_visible() {
                let now = Instant::now();
                let period = self.frame_period();
                if now.duration_since(self.due) >= period {
                    self.due = now.max(self.due + period);
                    let start = Instant::now();
                    self.advance();
                    self.advance_time += start.elapsed();
                    self.advance_calls += 1;
                }
                window.request_animation_frame();
            }

            let picture: AnyElement = match &self.current {
                Some(handle) => surface(handle.clone()).size_full().into_any_element(),
                None => div().size_full().bg(rgb(0x202024)).into_any_element(),
            };

            if std::env::var_os("PLAYER_TRACE").is_some() {
                let frames = self.metrics.borrow().frames;
                if frames > 0 && frames.is_multiple_of(60) && frames != self.reported_frames {
                    self.reported_frames = frames;
                    let metrics = *self.metrics.borrow();
                    let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / frames as f64;
                    let per_advance =
                        self.advance_time.as_secs_f64() * 1000.0 / self.advance_calls.max(1) as f64;
                    eprintln!(
                        "player: {frames} frames — evaluate {:.2}ms layout {:.2}ms paint {:.2}ms per frame; advance {per_advance:.2}ms/call; live={} signalled={}",
                        per_frame(metrics.evaluate),
                        per_frame(metrics.layout),
                        per_frame(metrics.paint),
                        self.playback.as_ref().map_or(0, Playback::live),
                        self.playback.as_ref().map_or(0, Playback::signalled),
                    );
                }
            }

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
