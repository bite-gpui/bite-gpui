//! Shaping, metric, and wrapping caches for text rendering.
//!
//! `TextSystem` wraps a platform text system with the engine's font-id,
//! metric, raster-bounds, and line-wrapper pools. It has no knowledge of
//! windows; the facade's window-scoped layer drives it.

use crate::LineWrapper;
use anyhow::{Context as _, Result, anyhow};
use collections::{FxHashMap, FxHashSet};
use gpui_engine::{
    Font, FontId, FontMetrics, FontRun, MissingGlyph, MissingGlyphSink, PlatformTextSystem,
    RenderGlyphParams, TextRenderingMode, font,
};
use gpui_types::{Bounds, DevicePixels, Hsla, Pixels, Size, px};
use itertools::Itertools;
use parking_lot::{Mutex, RwLock, RwLockUpgradableReadGuard};
use smallvec::{SmallVec, smallvec};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

// Leave enough room below the underline for its stroke while keeping it below the baseline.
const UNDERLINE_DESCENT_OFFSET_FACTOR: f32 = 0.618;

/// Returns the vertical offset used to paint an underline within a line.
pub fn underline_y_offset(line_height: Pixels, ascent: Pixels, descent: Pixels) -> Pixels {
    let padding_top = (line_height - ascent - descent) / 2.;
    padding_top + ascent + descent * UNDERLINE_DESCENT_OFFSET_FACTOR
}

const MAX_REPORTED_MISSING_GLYPHS: usize = 1024;

#[derive(Default)]
struct MissingGlyphState {
    reported: FxHashSet<MissingGlyph>,
    reported_order: VecDeque<MissingGlyph>,
    generation: usize,
}

impl MissingGlyphState {
    fn reset(&mut self, generation: usize) {
        self.reported.clear();
        self.reported_order.clear();
        self.generation = generation;
    }
}

struct QueuedMissingGlyph {
    generation: usize,
    missing_glyph: MissingGlyph,
}

/// Collects missing-glyph reports without invoking application code during layout.
struct MissingGlyphReporter {
    generation: Arc<AtomicUsize>,
    sender: async_channel::Sender<QueuedMissingGlyph>,
}

impl MissingGlyphSink for MissingGlyphReporter {
    fn report(&self, missing_glyphs: Vec<MissingGlyph>) {
        if self.sender.is_closed() {
            return;
        }

        let generation = self.generation.load(Ordering::Acquire);
        // Repetitions within a line must not fill the queue before its other
        // missing glyphs. Cross-report deduplication belongs to the receiver.
        for missing_glyph in missing_glyphs.into_iter().unique() {
            let queued = QueuedMissingGlyph {
                generation,
                missing_glyph,
            };
            if self.sender.try_send(queued).is_err() {
                break;
            }
        }
    }
}

impl MissingGlyphReporter {
    fn reset(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }
}

/// Receives batches of grapheme clusters that exhausted font fallback.
pub struct MissingGlyphReceiver {
    state: MissingGlyphState,
    generation: Arc<AtomicUsize>,
    receiver: async_channel::Receiver<QueuedMissingGlyph>,
}

impl MissingGlyphReceiver {
    /// Waits until at least one new missing glyph has been observed.
    ///
    /// # Errors
    ///
    /// Returns [`async_channel::RecvError`] if the reporting channel is closed.
    pub async fn recv(
        &mut self,
    ) -> std::result::Result<Vec<MissingGlyph>, async_channel::RecvError> {
        loop {
            let queued = self.receiver.recv().await?;
            let mut missing_glyphs = Vec::new();
            for queued in std::iter::once(queued)
                .chain(std::iter::from_fn(|| self.receiver.try_recv().ok()))
                .take(MAX_REPORTED_MISSING_GLYPHS)
            {
                let generation = self.generation.load(Ordering::Acquire);
                if self.state.generation != generation {
                    self.state.reset(generation);
                    missing_glyphs.clear();
                }
                if queued.generation != generation
                    || !self.state.reported.insert(queued.missing_glyph.clone())
                {
                    continue;
                }
                self.state
                    .reported_order
                    .push_back(queued.missing_glyph.clone());
                missing_glyphs.push(queued.missing_glyph);
                if self.state.reported.len() > MAX_REPORTED_MISSING_GLYPHS
                    && let Some(expired) = self.state.reported_order.pop_front()
                {
                    self.state.reported.remove(&expired);
                }
            }
            if !missing_glyphs.is_empty() {
                return Ok(missing_glyphs);
            }
            // A producer can keep refilling the queue with already-reported
            // glyphs. Bound work per poll even when every report is filtered out.
            let mut yielded = false;
            std::future::poll_fn(|cx| {
                if std::mem::replace(&mut yielded, true) {
                    std::task::Poll::Ready(())
                } else {
                    cx.waker().wake_by_ref();
                    std::task::Poll::Pending
                }
            })
            .await;
        }
    }
}

impl Drop for MissingGlyphReceiver {
    fn drop(&mut self) {
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
    }
}

/// The GPUI text rendering sub system.
pub struct TextSystem {
    platform_text_system: Arc<dyn PlatformTextSystem>,
    font_ids_by_font: RwLock<FxHashMap<Font, Result<FontId>>>,
    font_metrics: RwLock<FxHashMap<FontId, FontMetrics>>,
    raster_bounds: RwLock<FxHashMap<RenderGlyphParams, Bounds<DevicePixels>>>,
    wrapper_pool: Mutex<FxHashMap<FontIdWithSize, Vec<LineWrapper>>>,
    font_runs_pool: Mutex<Vec<Vec<FontRun>>>,
    fallback_font_stack: SmallVec<[Font; 2]>,
    font_generation: Arc<AtomicUsize>,
    missing_glyph_reporter: Arc<MissingGlyphReporter>,
    missing_glyph_receiver: Mutex<Option<MissingGlyphReceiver>>,
}

impl TextSystem {
    /// Create a new TextSystem with the given platform text system.
    pub fn new(platform_text_system: Arc<dyn PlatformTextSystem>) -> Self {
        let (sender, receiver) = async_channel::bounded(MAX_REPORTED_MISSING_GLYPHS);
        let missing_glyph_generation = Arc::<AtomicUsize>::default();
        TextSystem {
            platform_text_system,
            font_metrics: RwLock::default(),
            raster_bounds: RwLock::default(),
            font_ids_by_font: RwLock::default(),
            wrapper_pool: Mutex::default(),
            font_runs_pool: Mutex::default(),
            fallback_font_stack: smallvec![
                // TODO: Remove this when Linux have implemented setting fallbacks.
                font(".ZedMono"),
                font(".ZedSans"),
                font("Helvetica"),
                font("Segoe UI"),     // Windows
                font("Ubuntu"),       // Gnome (Ubuntu)
                font("Adwaita Sans"), // Gnome 47
                font("Cantarell"),    // Gnome
                font("Noto Sans"),    // KDE
                font("DejaVu Sans"),
                font("Arial"), // macOS, Windows
            ],
            font_generation: Arc::default(),
            missing_glyph_reporter: Arc::new(MissingGlyphReporter {
                generation: missing_glyph_generation.clone(),
                sender,
            }),
            missing_glyph_receiver: Mutex::new(Some(MissingGlyphReceiver {
                state: MissingGlyphState::default(),
                generation: missing_glyph_generation,
                receiver,
            })),
        }
    }

    /// The platform text system this engine wraps.
    pub fn platform_text_system(&self) -> &Arc<dyn PlatformTextSystem> {
        &self.platform_text_system
    }

    /// The generation that advances whenever [`Self::add_fonts`] installs fonts.
    pub fn font_generation(&self) -> &Arc<AtomicUsize> {
        &self.font_generation
    }

    /// Takes a pooled font-run buffer, or an empty one when the pool is dry.
    pub fn take_font_runs(&self) -> Vec<FontRun> {
        self.font_runs_pool.lock().pop().unwrap_or_default()
    }

    /// Returns a font-run buffer to the pool for reuse.
    pub fn recycle_font_runs(&self, font_runs: Vec<FontRun>) {
        self.font_runs_pool.lock().push(font_runs);
    }

    /// Get sorted, unique font family names available to the platform text system.
    ///
    /// Includes fonts registered with [`Self::add_fonts`].
    pub fn all_font_names(&self) -> Vec<String> {
        let mut names = self.platform_text_system.all_font_names();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Add a font's data to the text system.
    ///
    /// Cached font resolution and line layouts are invalidated after installation.
    /// Layouts already in progress may complete against the previous font set.
    pub fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.platform_text_system.add_fonts(fonts)?;
        self.font_ids_by_font.write().clear();
        self.missing_glyph_reporter.reset();
        self.font_generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Takes the receiver for missing-glyph reports.
    ///
    /// Only one receiver is available for each text system. Returns `None` when
    /// the receiver was already taken or another caller is taking it.
    pub fn take_missing_glyph_receiver(&self) -> Option<MissingGlyphReceiver> {
        self.missing_glyph_receiver
            .try_lock()
            .and_then(|mut receiver| receiver.take())
    }

    /// Starts reporting grapheme clusters that exhaust font fallback.
    pub fn enable_missing_glyph_reporting(&self) {
        self.platform_text_system
            .set_missing_glyph_sink(Some(self.missing_glyph_reporter.clone()));
    }

    /// Stops reporting missing glyphs and discards any reports collected so far.
    pub fn disable_missing_glyph_reporting(&self) {
        self.platform_text_system.set_missing_glyph_sink(None);
        self.missing_glyph_reporter.reset();
    }

    /// Reports missing glyphs as if the platform text system had observed them.
    #[cfg(any(test, feature = "test-support"))]
    pub fn report_missing_glyphs_in_test(&self, missing_glyphs: Vec<MissingGlyph>) {
        self.missing_glyph_reporter.report(missing_glyphs);
    }

    /// Get the FontId for the configure font family and style.
    fn font_id(&self, font: &Font) -> Result<FontId> {
        fn clone_font_id_result(font_id: &Result<FontId>) -> Result<FontId> {
            match font_id {
                Ok(font_id) => Ok(*font_id),
                Err(err) => Err(anyhow!("{err}")),
            }
        }

        let font_id = self
            .font_ids_by_font
            .read()
            .get(font)
            .map(clone_font_id_result);
        if let Some(font_id) = font_id {
            font_id
        } else {
            let font_id = self.platform_text_system.font_id(font);
            self.font_ids_by_font
                .write()
                .insert(font.clone(), clone_font_id_result(&font_id));
            font_id
        }
    }

    /// Get the Font for the Font Id.
    pub fn get_font_for_id(&self, id: FontId) -> Option<Font> {
        let lock = self.font_ids_by_font.read();
        lock.iter()
            .filter_map(|(font, result)| match result {
                Ok(font_id) if *font_id == id => Some(font.clone()),
                _ => None,
            })
            .next()
    }

    /// Resolves the specified font, falling back to the default font stack if
    /// the font fails to load.
    ///
    /// # Panics
    ///
    /// Panics if the font and none of the fallbacks can be resolved.
    pub fn resolve_font(&self, font: &Font) -> FontId {
        if let Ok(font_id) = self.font_id(font) {
            return font_id;
        }
        for fallback in &self.fallback_font_stack {
            if let Ok(font_id) = self.font_id(fallback) {
                return font_id;
            }
        }

        panic!(
            "failed to resolve font '{}' or any of the fallbacks: {}",
            font.family,
            self.fallback_font_stack
                .iter()
                .map(|fallback| &fallback.family)
                .join(", ")
        );
    }

    /// Prewarm any system font caches needed to shape text.
    ///
    /// This may be expensive, so callers should generally invoke it on a
    /// background executor. Missing entries are still populated on demand by
    /// the normal shaping path.
    pub fn prewarm_fonts(&self, fonts: &[Font]) {
        let mut font_ids = SmallVec::<[FontId; 8]>::new();
        for font in fonts {
            let font_id = self.resolve_font(font);
            if !font_ids.contains(&font_id) {
                font_ids.push(font_id);
            }
        }
        self.platform_text_system.prewarm_fonts(&font_ids);
    }

    /// Get the bounding box for the given font and font size.
    /// A font's bounding box is the smallest rectangle that could enclose all glyphs
    /// in the font. superimposed over one another.
    pub fn bounding_box(&self, font_id: FontId, font_size: Pixels) -> Bounds<Pixels> {
        self.read_metrics(font_id, |metrics| metrics.bounding_box(font_size))
    }

    /// Get the typographic bounds for the given character, in the given font and size.
    pub fn typographic_bounds(
        &self,
        font_id: FontId,
        font_size: Pixels,
        character: char,
    ) -> Result<Bounds<Pixels>> {
        let glyph_id = self
            .platform_text_system
            .glyph_for_char(font_id, character)
            .with_context(|| format!("glyph not found for character '{character}'"))?;
        let bounds = self
            .platform_text_system
            .typographic_bounds(font_id, glyph_id)?;
        Ok(self.read_metrics(font_id, |metrics| {
            (bounds / metrics.units_per_em as f32 * font_size.0).map(px)
        }))
    }

    /// Get the advance width for the given character, in the given font and size.
    pub fn advance(&self, font_id: FontId, font_size: Pixels, ch: char) -> Result<Size<Pixels>> {
        let glyph_id = self
            .platform_text_system
            .glyph_for_char(font_id, ch)
            .with_context(|| format!("glyph not found for character '{ch}'"))?;
        let result = self.platform_text_system.advance(font_id, glyph_id)?
            / self.units_per_em(font_id) as f32;

        Ok(result * font_size)
    }

    // Consider removing this?
    /// Returns the shaped layout width of for the given character, in the given font and size.
    pub fn layout_width(&self, font_id: FontId, font_size: Pixels, ch: char) -> Pixels {
        let mut buffer = [0; 4];
        let buffer = ch.encode_utf8(&mut buffer);
        self.platform_text_system
            .layout_line(
                buffer,
                font_size,
                &[FontRun {
                    len: buffer.len(),
                    font_id,
                }],
            )
            .width
    }

    /// Returns the width of an `em`.
    ///
    /// Uses the width of the `m` character in the given font and size.
    pub fn em_width(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.typographic_bounds(font_id, font_size, 'm')?.size.width)
    }

    /// Returns the advance width of an `em`.
    ///
    /// Uses the advance width of the `m` character in the given font and size.
    pub fn em_advance(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.advance(font_id, font_size, 'm')?.width)
    }

    /// Returns the width of an `ch`.
    ///
    /// Uses the width of the `0` character in the given font and size.
    pub fn ch_width(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.typographic_bounds(font_id, font_size, '0')?.size.width)
    }

    /// Returns the advance width of an `ch`.
    ///
    /// Uses the advance width of the `0` character in the given font and size.
    pub fn ch_advance(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        Ok(self.advance(font_id, font_size, '0')?.width)
    }

    /// Get the number of font size units per 'em square',
    /// Per MDN: "an abstract square whose height is the intended distance between
    /// lines of type in the same type size"
    pub fn units_per_em(&self, font_id: FontId) -> u32 {
        self.read_metrics(font_id, |metrics| metrics.units_per_em)
    }

    /// Get the height of a capital letter in the given font and size.
    pub fn cap_height(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.cap_height(font_size))
    }

    /// Get the height of the x character in the given font and size.
    pub fn x_height(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.x_height(font_size))
    }

    /// Get the recommended distance from the baseline for the given font
    pub fn ascent(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.ascent(font_size))
    }

    /// Get the recommended distance below the baseline for the given font,
    /// in single spaced text.
    pub fn descent(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.read_metrics(font_id, |metrics| metrics.descent(font_size))
    }

    /// Get the recommended baseline offset for the given font and line height.
    pub fn baseline_offset(
        &self,
        font_id: FontId,
        font_size: Pixels,
        line_height: Pixels,
    ) -> Pixels {
        let ascent = self.ascent(font_id, font_size);
        let descent = self.descent(font_id, font_size);
        let padding_top = (line_height - ascent - descent) / 2.;
        padding_top + ascent
    }

    fn read_metrics<T>(&self, font_id: FontId, read: impl FnOnce(&FontMetrics) -> T) -> T {
        let lock = self.font_metrics.upgradable_read();

        if let Some(metrics) = lock.get(&font_id) {
            read(metrics)
        } else {
            let mut lock = RwLockUpgradableReadGuard::upgrade(lock);
            let metrics = lock
                .entry(font_id)
                .or_insert_with(|| self.platform_text_system.font_metrics(font_id));
            read(metrics)
        }
    }

    /// Returns a handle to a line wrapper, for the given font and font size.
    pub fn line_wrapper(self: &Arc<Self>, font: Font, font_size: Pixels) -> LineWrapperHandle {
        let lock = &mut self.wrapper_pool.lock();
        let font_id = self.resolve_font(&font);
        let wrappers = lock
            .entry(FontIdWithSize { font_id, font_size })
            .or_default();
        let wrapper = wrappers
            .pop()
            .unwrap_or_else(|| LineWrapper::new(font_id, font_size, self.clone()));

        LineWrapperHandle {
            wrapper: Some(wrapper),
            text_system: self.clone(),
        }
    }

    /// Get the rasterized size and location of a specific, rendered glyph.
    pub fn raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        let raster_bounds = self.raster_bounds.upgradable_read();
        if let Some(bounds) = raster_bounds.get(params) {
            Ok(*bounds)
        } else {
            let mut raster_bounds = RwLockUpgradableReadGuard::upgrade(raster_bounds);
            let bounds = self.platform_text_system.glyph_raster_bounds(params)?;
            raster_bounds.insert(params.clone(), bounds);
            Ok(bounds)
        }
    }

    /// Rasterizes a glyph, returning its size and coverage bitmap.
    pub fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let raster_bounds = self.raster_bounds(params)?;
        self.platform_text_system
            .rasterize_glyph(params, raster_bounds)
    }

    /// Returns the dilation level to use for a glyph painted in the given color.
    pub fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.platform_text_system.glyph_dilation_for_color(color)
    }

    /// Returns the text rendering mode recommended by the platform for the given font and size.
    /// The return value will never be [`TextRenderingMode::PlatformDefault`].
    pub fn recommended_rendering_mode(
        &self,
        font_id: FontId,
        font_size: Pixels,
    ) -> TextRenderingMode {
        self.platform_text_system
            .recommended_rendering_mode(font_id, font_size)
    }
}

#[derive(Hash, Eq, PartialEq)]
struct FontIdWithSize {
    font_id: FontId,
    font_size: Pixels,
}

/// A handle into the text system, which can be used to compute the wrapped layout of text
pub struct LineWrapperHandle {
    wrapper: Option<LineWrapper>,
    text_system: Arc<TextSystem>,
}

impl Drop for LineWrapperHandle {
    fn drop(&mut self) {
        let mut state = self.text_system.wrapper_pool.lock();
        let wrapper = self.wrapper.take().unwrap();
        state
            .get_mut(&FontIdWithSize {
                font_id: wrapper.font_id,
                font_size: wrapper.font_size,
            })
            .unwrap()
            .push(wrapper);
    }
}

impl Deref for LineWrapperHandle {
    type Target = LineWrapper;

    fn deref(&self) -> &Self::Target {
        self.wrapper.as_ref().unwrap()
    }
}

impl DerefMut for LineWrapperHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.wrapper.as_mut().unwrap()
    }
}

#[cfg(test)]
mod missing_glyph_tests {
    use super::*;
    use futures::FutureExt as _;
    use gpui_engine::FallbackFontClass;

    #[test]
    fn bounds_retained_missing_glyphs() {
        let (reporter, mut receiver) = missing_glyph_channel();
        reporter.report(
            (0..MAX_REPORTED_MISSING_GLYPHS)
                .map(|index| {
                    MissingGlyph::new(index.to_string().into(), FallbackFontClass::Proportional)
                })
                .collect(),
        );
        assert!(receiver.recv().now_or_never().unwrap().is_ok());

        let newest = MissingGlyph::new("newest".into(), FallbackFontClass::Monospace);
        reporter.report(vec![newest.clone()]);
        assert!(receiver.recv().now_or_never().unwrap().is_ok());

        let state = &receiver.state;
        assert_eq!(state.reported.len(), MAX_REPORTED_MISSING_GLYPHS);
        assert_eq!(state.reported_order.len(), MAX_REPORTED_MISSING_GLYPHS);
        assert!(state.reported.contains(&newest));
    }

    #[test]
    fn dropping_receiver_closes_and_clears_reports() {
        let (reporter, receiver) = missing_glyph_channel();
        reporter.report(vec![missing_glyph("missing")]);

        drop(receiver);

        assert!(reporter.sender.is_closed());
        assert!(reporter.sender.is_empty());
    }

    fn missing_glyph_channel() -> (MissingGlyphReporter, MissingGlyphReceiver) {
        let (sender, receiver) = async_channel::bounded(MAX_REPORTED_MISSING_GLYPHS);
        let generation = Arc::<AtomicUsize>::default();
        let reporter = MissingGlyphReporter {
            generation: generation.clone(),
            sender,
        };
        let receiver = MissingGlyphReceiver {
            state: MissingGlyphState::default(),
            generation,
            receiver,
        };
        (reporter, receiver)
    }

    fn missing_glyph(grapheme: &'static str) -> MissingGlyph {
        MissingGlyph::new(grapheme.into(), FallbackFontClass::Proportional)
    }
}
