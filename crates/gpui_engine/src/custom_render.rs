use std::{any::Any, fmt, sync::Arc};

use crate::{BorderStyle, Quad};
use gpui_types::{Background, Bounds, ContentMask, Corners, DrawOrder, Edges, Hsla, ScaledPixels};

/// A token for a texture produced outside GPUI, on the renderer's own device.
///
/// The payload is erased because the handle has exactly one counterparty — the application
/// that made the texture and the renderer that application chose — and those two agree on the
/// concrete type it holds. Keeping the engine blind to that type is what keeps `wgpu` and the
/// platform graphics APIs out of this crate. It is also why there is no id and no registry
/// beside it: the handle *is* the resource, so it cannot be wrong.
#[derive(Clone)]
pub struct ImportedTextureHandle {
    /// The backend's own payload, and the one thing the engine never looks at.
    pub payload: Arc<dyn Any + Send + Sync>,
}

impl ImportedTextureHandle {
    /// Wrap a backend payload — a `wgpu::TextureView`, a `MetalTexture` — as a handle.
    pub fn new(payload: impl Any + Send + Sync) -> Self {
        Self {
            payload: Arc::new(payload),
        }
    }
}

impl fmt::Debug for ImportedTextureHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ImportedTextureHandle")
    }
}

/// A drawable the application produced itself, drawn in the composite pass rather than
/// translated into GPUI's own primitives.
///
/// It rides [`crate::PaintSurface`]'s machinery — the ordering, the content mask, one
/// accumulation list and one batch — but is drawn by a fragment path of its own: what it
/// samples is RGBA, where that surface is YCbCr video.
#[derive(Clone)]
pub enum CustomRenderPrimitive {
    /// A texture produced outside GPUI, sampled in the composite pass.
    Texture {
        /// Assigned by the scene, so the texture composites with its siblings in order.
        order: DrawOrder,
        /// The token the renderer resolves to the texture it samples.
        handle: ImportedTextureHandle,
        /// Where the texture lands, in scaled pixels.
        bounds: Bounds<ScaledPixels>,
        /// What may clip it.
        content_mask: ContentMask<ScaledPixels>,
        /// Corner radii, in scaled pixels: the element converts at paint time, as `paint_quad`
        /// does, because the renderer has no scale factor of its own.
        radii: Corners<ScaledPixels>,
        /// Multiplied into the sample.
        opacity: f32,
        /// The producer's textures have a top-left origin; the scene's quads do not.
        flip_v: bool,
    },
    /* Inline, in inline-commands.md */
}

impl CustomRenderPrimitive {
    /// The order the scene assigned it.
    pub fn order(&self) -> DrawOrder {
        match self {
            Self::Texture { order, .. } => *order,
        }
    }

    /// The order, for the scene to assign.
    pub fn order_mut(&mut self) -> &mut DrawOrder {
        match self {
            Self::Texture { order, .. } => order,
        }
    }

    /// Where it lands.
    pub fn bounds(&self) -> &Bounds<ScaledPixels> {
        match self {
            Self::Texture { bounds, .. } => bounds,
        }
    }

    /// What may clip it.
    pub fn content_mask(&self) -> &ContentMask<ScaledPixels> {
        match self {
            Self::Texture { content_mask, .. } => content_mask,
        }
    }

    /// Encode this primitive into the quad instance record the backends' shaders read.
    ///
    /// Both renderers draw it through their quad pipeline, so its geometry, its
    /// content-mask clip and its corner SDF are the quads' rather than a second
    /// implementation of them. That leaves two values with no field of their own: the
    /// opacity rides in the solid background's alpha, which the quad vertex entry point
    /// forwards as a scratch field the texturing path does not read, and `flip_v` rides in
    /// the border style, which a textured quad has no other use for. Encoding it here
    /// rather than in each renderer is what keeps the two in step with the shaders.
    pub fn to_quad_record(&self) -> Quad {
        match self {
            Self::Texture {
                order,
                bounds,
                content_mask,
                radii,
                opacity,
                flip_v,
                ..
            } => {
                let mut background = Background::default();
                background.solid = Hsla {
                    h: 0.0,
                    s: 0.0,
                    l: 0.0,
                    a: *opacity,
                };

                Quad {
                    order: *order,
                    border_style: if *flip_v {
                        BorderStyle::Dashed
                    } else {
                        BorderStyle::Solid
                    },
                    bounds: *bounds,
                    content_mask: *content_mask,
                    background,
                    border_color: Hsla::default(),
                    corner_radii: *radii,
                    border_widths: Edges::default(),
                }
            }
        }
    }
}

impl fmt::Debug for CustomRenderPrimitive {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Texture {
                handle,
                bounds,
                opacity,
                flip_v,
                ..
            } => formatter
                .debug_struct("Texture")
                .field("handle", handle)
                .field("bounds", bounds)
                .field("opacity", opacity)
                .field("flip_v", flip_v)
                .finish_non_exhaustive(),
        }
    }
}

/// A native texture handle for the Metal backend.
///
/// This is what a macOS renderer resolves [`ImportedTextureHandle`] against: a raw
/// `id<MTLTexture>`, which is not `Send`, but the handle crosses no thread. The assertions are
/// only what [`ImportedTextureHandle`]'s `Arc<dyn Any + Send + Sync>` asks for.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
pub struct MetalTexture(pub *mut std::ffi::c_void);

#[cfg(target_os = "macos")]
unsafe impl Send for MetalTexture {}

#[cfg(target_os = "macos")]
unsafe impl Sync for MetalTexture {}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_types::{Point, Size};

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: Point {
                x: ScaledPixels(x),
                y: ScaledPixels(y),
            },
            size: Size {
                width: ScaledPixels(width),
                height: ScaledPixels(height),
            },
        }
    }

    fn texture(bounds: Bounds<ScaledPixels>, radii: Corners<ScaledPixels>, opacity: f32, flip_v: bool) -> CustomRenderPrimitive {
        CustomRenderPrimitive::Texture {
            order: 0,
            handle: ImportedTextureHandle::new(()),
            bounds,
            content_mask: ContentMask { bounds },
            radii,
            opacity,
            flip_v,
        }
    }

    /// The record the imported-texture pipeline reads is the quad record, so the geometry, the
    /// clip and the radii have to survive the encoding, and the two values with no field of their
    /// own have to land in the fields the quad path does not otherwise use. Both shaders read it
    /// that way, which is why it is written down once, here.
    #[test]
    fn a_texture_primitive_encodes_into_the_quad_record() {
        let bounds = bounds(1.0, 2.0, 3.0, 4.0);
        let radii = Corners {
            top_left: ScaledPixels(5.0),
            top_right: ScaledPixels(6.0),
            bottom_right: ScaledPixels(7.0),
            bottom_left: ScaledPixels(8.0),
        };

        let quad = texture(bounds, radii, 0.25, true).to_quad_record();

        assert_eq!(quad.bounds, bounds);
        assert_eq!(quad.content_mask, ContentMask { bounds });
        assert_eq!(quad.corner_radii, radii);
        assert_eq!(
            quad.background.solid.a, 0.25,
            "the opacity rides in the solid background's alpha"
        );
        assert_eq!(
            quad.border_style,
            BorderStyle::Dashed,
            "flip_v rides in the border style"
        );
    }

    /// A producer whose texture already has GPUI's origin must not be flipped, so the encoding
    /// has to distinguish the two cases rather than always marking the record.
    #[test]
    fn a_texture_primitive_without_a_flip_encodes_a_solid_border_style() {
        let bounds = bounds(0.0, 0.0, 10.0, 10.0);

        let quad = texture(bounds, Corners::default(), 1.0, false).to_quad_record();

        assert_eq!(quad.border_style, BorderStyle::Solid);
        assert_eq!(quad.background.solid.a, 1.0);
    }
}
