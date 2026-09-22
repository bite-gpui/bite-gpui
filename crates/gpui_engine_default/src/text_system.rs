//! The default text system: font resolution, metrics, pooling, and the per-frame
//! line-layout cache over any [`PlatformTextSystem`].
//!
//! It has no knowledge of windows; the facade's window-scoped layer drives it.

use crate::LineLayoutCache;
use anyhow::{Context as _, Result, anyhow};
use collections::{FxHashMap, FxHashSet};
use gpui_engine::{
    Font, FontId, FontMetrics, FontRun, LineLayout, LineLayoutIndex, LineWrapper,
    LineWrapperHandle, MissingGlyph, MissingGlyphReports, MissingGlyphSink, PlatformTextSystem,
    RenderGlyphParams, TextRenderingMode, TextSystem, WrappedLineLayout, font,
};
use gpui_shared_string::SharedString;
use gpui_types::{Bounds, DevicePixels, Hsla, Pixels, Size, px};
use itertools::Itertools;
use parking_lot::{Mutex, RwLock, RwLockUpgradableReadGuard};
use smallvec::{SmallVec, smallvec};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

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

impl MissingGlyphReports for MissingGlyphReceiver {
    fn recv(&mut self) -> Pin<Box<dyn Future<Output = Option<Vec<MissingGlyph>>> + Send + '_>> {
        Box::pin(async move { MissingGlyphReceiver::recv(self).await.ok() })
    }
}

/// A key pairing a resolved font with the size it was resolved at, for the
/// per-size wrapper pool.
#[derive(Hash, Eq, PartialEq)]
pub struct FontIdWithSize {
    font_id: FontId,
    font_size: Pixels,
}

/// The GPUI text rendering sub system.
pub struct DefaultTextSystem {
    platform_text_system: Arc<dyn PlatformTextSystem>,
    font_ids_by_font: RwLock<FxHashMap<Font, Result<FontId>>>,
    font_metrics: RwLock<FxHashMap<FontId, FontMetrics>>,
    raster_bounds: RwLock<FxHashMap<RenderGlyphParams, Bounds<DevicePixels>>>,
    wrapper_pool: Mutex<FxHashMap<FontIdWithSize, Vec<LineWrapper>>>,
    font_runs_pool: Mutex<Vec<Vec<FontRun>>>,
    fallback_font_stack: SmallVec<[Font; 2]>,
    missing_glyph_reporter: Arc<MissingGlyphReporter>,
    missing_glyph_receiver: Mutex<Option<MissingGlyphReceiver>>,
    line_layout_cache: LineLayoutCache,
}

impl DefaultTextSystem {
    /// Create a new DefaultTextSystem with the given platform text system.
    pub fn new(platform_text_system: Arc<dyn PlatformTextSystem>) -> Self {
        let line_layout_cache = LineLayoutCache::new(platform_text_system.clone());
        let (sender, receiver) = async_channel::bounded(MAX_REPORTED_MISSING_GLYPHS);
        let missing_glyph_generation = Arc::<AtomicUsize>::default();
        DefaultTextSystem {
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
            missing_glyph_reporter: Arc::new(MissingGlyphReporter {
                generation: missing_glyph_generation.clone(),
                sender,
            }),
            missing_glyph_receiver: Mutex::new(Some(MissingGlyphReceiver {
                state: MissingGlyphState::default(),
                generation: missing_glyph_generation,
                receiver,
            })),
            line_layout_cache,
        }
    }

    /// The platform text system this engine wraps.
    pub fn platform_text_system(&self) -> &Arc<dyn PlatformTextSystem> {
        &self.platform_text_system
    }

    /// Take a pooled font-run buffer, or an empty one when the pool is dry.
    pub fn take_font_runs(&self) -> Vec<FontRun> {
        self.font_runs_pool.lock().pop().unwrap_or_default()
    }

    /// Return a font-run buffer to the pool for reuse.
    pub fn recycle_font_runs(&self, font_runs: Vec<FontRun>) {
        self.font_runs_pool.lock().push(font_runs);
    }

    /// Get a list of all available font names from the operating system.
    pub fn all_font_names(&self) -> Vec<String> {
        let mut names = self.platform_text_system.all_font_names();
        names.extend(
            self.fallback_font_stack
                .iter()
                .map(|font| font.family.to_string()),
        );
        names.push(".SystemUIFont".to_string());
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Add a font's data to the text system.
    pub fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        // Serialize registration with cache misses so an in-flight lookup cannot
        // repopulate a stale miss after the newly registered fonts become available.
        let mut font_ids = self.font_ids_by_font.write();
        let result = self.platform_text_system.add_fonts(fonts);
        font_ids.clear();
        result
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
            let mut font_ids = self.font_ids_by_font.write();
            if let Some(font_id) = font_ids.get(font) {
                return clone_font_id_result(font_id);
            }
            let font_id = self.platform_text_system.font_id(font);
            font_ids.insert(font.clone(), clone_font_id_result(&font_id));
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
                    letter_spacing: None,
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
    pub fn line_wrapper(self: Arc<Self>, font: Font, font_size: Pixels) -> LineWrapperHandle {
        let font_id = self.resolve_font(&font);
        let wrapper = {
            let mut lock = self.wrapper_pool.lock();
            lock.entry(FontIdWithSize { font_id, font_size })
                .or_default()
                .pop()
                .unwrap_or_else(|| LineWrapper::new(font_id, font_size, self.clone()))
        };
        LineWrapperHandle::new(wrapper, move |mut wrapper| {
            wrapper.set_letter_spacing(None);
            let mut lock = self.wrapper_pool.lock();
            lock.get_mut(&FontIdWithSize { font_id, font_size })
                .expect("wrapper pool entry exists")
                .push(wrapper);
        })
    }

    /// Get the rasterized size and location of a specific, rendered glyph.
    pub(crate) fn raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
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

    pub(crate) fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        let raster_bounds = self.raster_bounds(params)?;
        self.platform_text_system
            .rasterize_glyph(params, raster_bounds)
    }

    /// Returns the dilation level to use for a glyph painted in the given color.
    pub(crate) fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.platform_text_system.glyph_dilation_for_color(color)
    }

    /// Returns the text rendering mode recommended by the platform for the given font and size.
    /// The return value will never be [`TextRenderingMode::PlatformDefault`].
    pub(crate) fn recommended_rendering_mode(
        &self,
        font_id: FontId,
        font_size: Pixels,
    ) -> TextRenderingMode {
        self.platform_text_system
            .recommended_rendering_mode(font_id, font_size)
    }
}
impl TextSystem for DefaultTextSystem {
    fn take_missing_glyph_receiver(&self) -> Option<Box<dyn MissingGlyphReports>> {
        self.missing_glyph_receiver
            .try_lock()
            .and_then(|mut receiver| receiver.take())
            .map(|receiver| Box::new(receiver) as Box<dyn MissingGlyphReports>)
    }

    fn enable_missing_glyph_reporting(&self) {
        self.platform_text_system
            .set_missing_glyph_sink(Some(self.missing_glyph_reporter.clone()));
    }

    fn disable_missing_glyph_reporting(&self) {
        self.platform_text_system.set_missing_glyph_sink(None);
        self.missing_glyph_reporter.reset();
    }

    fn platform_text_system(&self) -> &Arc<dyn PlatformTextSystem> {
        &self.platform_text_system
    }

    fn all_font_names(&self) -> Vec<String> {
        self.all_font_names()
    }

    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.add_fonts(fonts)
    }

    fn get_font_for_id(&self, id: FontId) -> Option<Font> {
        self.get_font_for_id(id)
    }

    fn resolve_font(&self, font: &Font) -> FontId {
        self.resolve_font(font)
    }

    fn prewarm_fonts(&self, fonts: &[Font]) {
        self.prewarm_fonts(fonts)
    }

    fn bounding_box(&self, font_id: FontId, font_size: Pixels) -> Bounds<Pixels> {
        self.bounding_box(font_id, font_size)
    }

    fn typographic_bounds(
        &self,
        font_id: FontId,
        font_size: Pixels,
        character: char,
    ) -> Result<Bounds<Pixels>> {
        self.typographic_bounds(font_id, font_size, character)
    }

    fn advance(&self, font_id: FontId, font_size: Pixels, ch: char) -> Result<Size<Pixels>> {
        self.advance(font_id, font_size, ch)
    }

    fn layout_width(&self, font_id: FontId, font_size: Pixels, ch: char) -> Pixels {
        self.layout_width(font_id, font_size, ch)
    }

    fn em_width(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        self.em_width(font_id, font_size)
    }

    fn em_advance(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        self.em_advance(font_id, font_size)
    }

    fn ch_width(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        self.ch_width(font_id, font_size)
    }

    fn ch_advance(&self, font_id: FontId, font_size: Pixels) -> Result<Pixels> {
        self.ch_advance(font_id, font_size)
    }

    fn units_per_em(&self, font_id: FontId) -> u32 {
        self.units_per_em(font_id)
    }

    fn cap_height(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.cap_height(font_id, font_size)
    }

    fn x_height(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.x_height(font_id, font_size)
    }

    fn ascent(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.ascent(font_id, font_size)
    }

    fn descent(&self, font_id: FontId, font_size: Pixels) -> Pixels {
        self.descent(font_id, font_size)
    }

    fn baseline_offset(&self, font_id: FontId, font_size: Pixels, line_height: Pixels) -> Pixels {
        self.baseline_offset(font_id, font_size, line_height)
    }

    fn take_font_runs(&self) -> Vec<FontRun> {
        self.take_font_runs()
    }

    fn recycle_font_runs(&self, font_runs: Vec<FontRun>) {
        self.recycle_font_runs(font_runs)
    }

    fn line_wrapper(self: Arc<Self>, font: Font, font_size: Pixels) -> LineWrapperHandle {
        DefaultTextSystem::line_wrapper(self, font, font_size)
    }

    fn raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.raster_bounds(params)
    }

    fn rasterize_glyph(&self, params: &RenderGlyphParams) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.rasterize_glyph(params)
    }

    fn glyph_dilation_for_color(&self, color: Hsla) -> u8 {
        self.glyph_dilation_for_color(color)
    }

    fn recommended_rendering_mode(&self, font_id: FontId, font_size: Pixels) -> TextRenderingMode {
        self.recommended_rendering_mode(font_id, font_size)
    }

    fn layout_index(&self) -> LineLayoutIndex {
        self.line_layout_cache.layout_index()
    }

    fn reuse_layouts(&self, range: Range<LineLayoutIndex>) {
        self.line_layout_cache.reuse_layouts(range)
    }

    fn truncate_layouts(&self, index: LineLayoutIndex) {
        self.line_layout_cache.truncate_layouts(index)
    }

    fn finish_frame(&self) {
        self.line_layout_cache.finish_frame()
    }

    fn layout_wrapped_line(
        &self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        wrap_width: Option<Pixels>,
        max_lines: Option<usize>,
    ) -> Arc<WrappedLineLayout> {
        self.line_layout_cache
            .layout_wrapped_line(text, font_size, runs, wrap_width, max_lines)
    }

    fn layout_line(
        &self,
        text: &str,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Arc<LineLayout> {
        self.line_layout_cache
            .layout_line(text, font_size, runs, force_width)
    }

    fn try_layout_line_by_hash(
        &self,
        text_hash: u64,
        text_len: usize,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
    ) -> Option<Arc<LineLayout>> {
        self.line_layout_cache.try_layout_line_by_hash(
            text_hash,
            text_len,
            font_size,
            runs,
            force_width,
        )
    }

    fn layout_line_by_hash(
        &self,
        text_hash: u64,
        text_len: usize,
        font_size: Pixels,
        runs: &[FontRun],
        force_width: Option<Pixels>,
        materialize_text: Box<dyn FnOnce() -> SharedString>,
    ) -> Arc<LineLayout> {
        self.line_layout_cache.layout_line_by_hash(
            text_hash,
            text_len,
            font_size,
            runs,
            force_width,
            materialize_text,
        )
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
