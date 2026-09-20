//! Graphical filters, in both their logical-pixel form and the scene-space form the
//! renderers consume.

use gpui_types::{Pixels, ScaledPixels};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A graphical filter that can be applied either to an element's own content
/// (via `Styled::filter`, like CSS `filter`) or to the content rendered behind
/// it (via `Styled::backdrop_filter`, like CSS `backdrop-filter`). The styling
/// methods live on the facade above this crate, so they are not linked here.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum Filter {
    /// A gaussian blur with the given radius, in logical pixels. Maps to CSS `blur(<px>)`.
    Blur(Pixels),
}

impl Filter {
    /// Whether this filter has no visible effect, so painting can skip it entirely (and the
    /// element can avoid the offscreen isolation pass when *all* of its filters are identities).
    ///
    /// Each variant declares its own no-op case here rather than the pipeline special-casing
    /// blur — adding a filter that this returns `true` for is silently dropped before it ever
    /// reaches the renderer.
    pub fn is_identity(&self) -> bool {
        match self {
            Filter::Blur(radius) => *radius <= Pixels::ZERO,
        }
    }

    /// Lower this logical-pixel filter into its scene-space ([`ScaledFilter`]) form for the
    /// renderer, scaling any pixel magnitudes by `factor` (the window scale factor).
    pub fn scale(&self, factor: f32) -> ScaledFilter {
        match self {
            Filter::Blur(radius) => ScaledFilter::Blur(radius.scale(factor)),
        }
    }
}

/// The scene-space (device-pixel) form of a [`Filter`], carried on the scene primitives that the
/// renderers consume. Produced by [`Filter::scale`]; pixel magnitudes are in [`ScaledPixels`].
///
/// This is intentionally a separate enum from [`Filter`] (rather than reusing it) so the scene
/// stays in device space like every other primitive, and so the renderers `match` on it
/// exhaustively — adding a filter variant breaks each backend's match, forcing a deliberate
/// implement-or-decline decision per backend instead of silently rendering nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScaledFilter {
    /// A gaussian blur with the given radius, in scaled (device) pixels.
    Blur(ScaledPixels),
}
