//! The surface-format vocabulary: every dimension a GPU needs to sample a buffer GPUI did not draw.
//!
//! A producer describes the pixels it made so a renderer can sample them. That description is a
//! *shape* — how the colour is modelled and subsampled, how the planes are laid out, the component
//! order, the sample depth and the word it is stored in, and what the alpha means. [`SurfaceFormat`]
//! holds that shape, one private dimension per field; it is built only here, so a `SurfaceFormat`
//! always describes a shape that exists.
//!
//! A producer reaches one through [`SurfaceFormatKind`], an enum that is **a choice of pre-constructed
//! `SurfaceFormat`s**: each variant names one format a producer emits and carries the shape that
//! format is. That gives a renderer a closed, exhaustive set to match on — and a format the vocabulary
//! does not contain cannot be named at all — while the dimensions stay a plain struct a renderer reads.
//!
//! What is modelled, and why the model stops where it does, is the `frame-formats.md` chapter under
//! `spi/rendering/`. Two families are deliberately **not** kinds because this model cannot name them
//! honestly: **packed YCbCr** (`YUYV`, `Y210`), whose component interleave is a dimension of its own,
//! and **sub-byte packed RGB** (`RGB565`, `A2R10G10B10`), whose components are not all the same width.
//! Both are curated in the chapter; neither is a [`SurfaceFormatKind`] yet.

/// How a surface's colour is modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorModel {
    /// Luma and chroma, converted to RGB before it is displayed.
    Yuv,
    /// RGB, sampled as it is.
    Rgb,
}

/// How much colour detail a chroma component carries relative to luma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsampling {
    /// No chroma: a single luma plane.
    C400,
    /// 4:1:1 — chroma at a quarter of the horizontal resolution.
    C411,
    /// 4:2:0 — chroma at half the resolution in both axes.
    C420,
    /// 4:2:2 — chroma at half the horizontal resolution.
    C422,
    /// 4:4:4 — chroma at the full resolution.
    C444,
}

/// How a surface's planes are arranged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneLayout {
    /// One plane, the components interleaved.
    Packed,
    /// A luma plane, then both chroma components interleaved in a second.
    SemiPlanar,
    /// A luma plane, then one plane for each chroma component.
    Planar,
}

/// The order of an RGB surface's first two components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbOrder {
    /// Red, then green (`RGBA8`).
    Rgb,
    /// Blue, then green (`BGRA8`).
    Bgr,
}

/// The order of a surface's chroma components, where they share a plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaOrder {
    /// Cb before Cr (`NV12`, `I420`).
    CbCr,
    /// Cr before Cb (`NV21`, `YV12`).
    CrCb,
}

/// The nominal bits of a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleDepth {
    /// 8 bits.
    Bits8,
    /// 10 bits.
    Bits10,
    /// 12 bits.
    Bits12,
    /// 16 bits.
    Bits16,
}

/// The word a sample is stored in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleContainer {
    /// One 8-bit byte, holding the whole sample.
    U8,
    /// A 16-bit word; a sample narrower than 16 bits sits in its **high** bits (`P010`, `P012`), as
    /// every 16-bit-container video format does.
    U16,
    /// A 32-bit word holding a packed sample (`Y410`).
    U32Packed,
}

/// What a surface's alpha component means, where it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alpha {
    /// No alpha: the surface is opaque.
    None,
    /// Straight (non-premultiplied) alpha.
    Straight,
    /// Premultiplied alpha.
    Premultiplied,
}

/// Every dimension of a surface format: the full shape a GPU importer needs to sample a buffer.
///
/// Built only here — its fields are private and its constructors are private — so it is always one of
/// the shapes a [`SurfaceFormatKind`] admits, and a renderer reads it rather than inventing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceFormat {
    model: ColorModel,
    subsampling: Subsampling,
    layout: PlaneLayout,
    rgb_order: RgbOrder,
    chroma_order: ChromaOrder,
    depth: SampleDepth,
    container: SampleContainer,
    alpha: Alpha,
}

impl SurfaceFormat {
    /// The RGB shape: one interleaved plane, full "chroma" resolution by definition.
    const fn rgb(
        order: RgbOrder,
        depth: SampleDepth,
        container: SampleContainer,
        alpha: Alpha,
    ) -> Self {
        Self {
            model: ColorModel::Rgb,
            subsampling: Subsampling::C444,
            layout: PlaneLayout::Packed,
            rgb_order: order,
            chroma_order: ChromaOrder::CbCr,
            depth,
            container,
            alpha,
        }
    }

    /// The YCbCr shape.
    const fn yuv(
        subsampling: Subsampling,
        layout: PlaneLayout,
        order: ChromaOrder,
        depth: SampleDepth,
        container: SampleContainer,
    ) -> Self {
        Self {
            model: ColorModel::Yuv,
            subsampling,
            layout,
            rgb_order: RgbOrder::Rgb,
            chroma_order: order,
            depth,
            container,
            alpha: Alpha::None,
        }
    }

    /// How the colour is modelled.
    pub const fn model(&self) -> ColorModel {
        self.model
    }

    /// How much colour detail the chroma carries.
    pub const fn subsampling(&self) -> Subsampling {
        self.subsampling
    }

    /// How the planes are arranged.
    pub const fn layout(&self) -> PlaneLayout {
        self.layout
    }

    /// The order of an RGB surface's first two components; meaningless for YCbCr.
    pub const fn rgb_order(&self) -> RgbOrder {
        self.rgb_order
    }

    /// The order of the chroma components; meaningless for RGB.
    pub const fn chroma_order(&self) -> ChromaOrder {
        self.chroma_order
    }

    /// The nominal bits of a sample.
    pub const fn depth(&self) -> SampleDepth {
        self.depth
    }

    /// The word a sample is stored in.
    pub const fn container(&self) -> SampleContainer {
        self.container
    }

    /// What the alpha component means, where the surface has one.
    pub const fn alpha(&self) -> Alpha {
        self.alpha
    }

    /// Whether the colour is YCbCr, and so needs a matrix before it is RGB.
    pub const fn is_yuv(&self) -> bool {
        matches!(self.model, ColorModel::Yuv)
    }

    /// How many planes a buffer of this shape carries: one for packed and mono, two for semi-planar,
    /// three for planar.
    pub const fn plane_count(&self) -> usize {
        match self.subsampling {
            Subsampling::C400 => 1,
            _ => match self.layout {
                PlaneLayout::Packed => 1,
                PlaneLayout::SemiPlanar => 2,
                PlaneLayout::Planar => 3,
            },
        }
    }
}

/// A format a producer emits: a choice of pre-constructed [`SurfaceFormat`]s.
///
/// See the module documentation. Name one of the constants ([`NV12`](Self::NV12) and friends) to get
/// a value; match a variant to tell formats apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceFormatKind {
    /// `BGRA8`: 8-bit blue/green/red/alpha, straight alpha — the common desktop format.
    Bgra8(SurfaceFormat),
    /// `RGBA8`: 8-bit red/green/blue/alpha, straight alpha.
    Rgba8(SurfaceFormat),

    /// `NV12`: 4:2:0 semi-planar, Cb then Cr — the universal decoder default.
    Nv12(SurfaceFormat),
    /// `NV21`: `NV12` with the chroma components the other way round.
    Nv21(SurfaceFormat),
    /// `I420` (`YU12`): 4:2:0 planar, Cb and Cr in their own planes after luma.
    I420(SurfaceFormat),
    /// `YV12`: `I420` with the two chroma planes the other way round.
    Yv12(SurfaceFormat),
    /// `NV16`: 4:2:2 semi-planar, Cb then Cr.
    Nv16(SurfaceFormat),
    /// `NV61`: `NV16` with the chroma components the other way round.
    Nv61(SurfaceFormat),
    /// `YU16`: 4:2:2 planar, Cb then Cr planes.
    Yu16(SurfaceFormat),
    /// `YV16`: `YU16` with the two chroma planes the other way round.
    Yv16(SurfaceFormat),
    /// `NV24`: 4:4:4 semi-planar, Cb then Cr.
    Nv24(SurfaceFormat),
    /// `YUV444`: 4:4:4 planar.
    Yuv444(SurfaceFormat),
    /// Mono: a single 8-bit luma plane and no chroma at all.
    Mono8(SurfaceFormat),

    /// `P010`: 4:2:0 semi-planar, 10 bits in the high bits of a 16-bit word — `HEVC Main10`,
    /// `VP9` profile 2 and `AV1`.
    P010(SurfaceFormat),
    /// `P012`: 4:2:0 semi-planar, 12 bits in a 16-bit word.
    P012(SurfaceFormat),
    /// `P016`: 4:2:0 semi-planar, a full 16-bit sample.
    P016(SurfaceFormat),
    /// `I010`: 4:2:0 planar, 10 bits in a 16-bit word.
    I010(SurfaceFormat),
    /// `I012`: 4:2:0 planar, 12 bits in a 16-bit word.
    I012(SurfaceFormat),
    /// `I016`: 4:2:0 planar, a full 16-bit sample.
    I016(SurfaceFormat),
    /// `P210`: 4:2:2 semi-planar, 10 bits in a 16-bit word.
    P210(SurfaceFormat),
    /// `P212`: 4:2:2 semi-planar, 12 bits in a 16-bit word.
    P212(SurfaceFormat),
    /// `P216`: 4:2:2 semi-planar, a full 16-bit sample.
    P216(SurfaceFormat),
    /// `P410`: 4:4:4 semi-planar, 10 bits in a 16-bit word.
    P410(SurfaceFormat),
    /// `P412`: 4:4:4 semi-planar, 12 bits in a 16-bit word.
    P412(SurfaceFormat),
    /// `P416`: 4:4:4 semi-planar, a full 16-bit sample.
    P416(SurfaceFormat),
    /// `Q410`: 4:4:4 planar, 10 bits in a 16-bit word.
    Q410(SurfaceFormat),
    /// `Q416`: 4:4:4 planar, a full 16-bit sample.
    Q416(SurfaceFormat),
}

impl SurfaceFormatKind {
    /// `BGRA8`: 8-bit blue/green/red/alpha, straight alpha.
    pub const fn bgra8() -> Self {
        Self::Bgra8(SurfaceFormat::rgb(
            RgbOrder::Bgr,
            SampleDepth::Bits8,
            SampleContainer::U8,
            Alpha::Straight,
        ))
    }

    /// `RGBA8`: 8-bit red/green/blue/alpha, straight alpha.
    pub const fn rgba8() -> Self {
        Self::Rgba8(SurfaceFormat::rgb(
            RgbOrder::Rgb,
            SampleDepth::Bits8,
            SampleContainer::U8,
            Alpha::Straight,
        ))
    }

    /// `NV12`: 4:2:0 semi-planar, Cb then Cr — the universal decoder default.
    pub const fn nv12() -> Self {
        Self::Nv12(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `NV21`: `NV12` with the chroma components the other way round.
    pub const fn nv21() -> Self {
        Self::Nv21(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CrCb,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `I420` (`YU12`): 4:2:0 planar, Cb and Cr in their own planes after luma.
    pub const fn i420() -> Self {
        Self::I420(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `YV12`: `I420` with the two chroma planes the other way round.
    pub const fn yv12() -> Self {
        Self::Yv12(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::Planar,
            ChromaOrder::CrCb,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `NV16`: 4:2:2 semi-planar, Cb then Cr.
    pub const fn nv16() -> Self {
        Self::Nv16(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `NV61`: `NV16` with the chroma components the other way round.
    pub const fn nv61() -> Self {
        Self::Nv61(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CrCb,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `YU16`: 4:2:2 planar, Cb then Cr planes.
    pub const fn yu16() -> Self {
        Self::Yu16(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `YV16`: `YU16` with the two chroma planes the other way round.
    pub const fn yv16() -> Self {
        Self::Yv16(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::Planar,
            ChromaOrder::CrCb,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `NV24`: 4:4:4 semi-planar, Cb then Cr.
    pub const fn nv24() -> Self {
        Self::Nv24(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `YUV444`: 4:4:4 planar.
    pub const fn yuv444() -> Self {
        Self::Yuv444(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// Mono: a single 8-bit luma plane and no chroma at all.
    pub const fn mono8() -> Self {
        Self::Mono8(SurfaceFormat::yuv(
            Subsampling::C400,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits8,
            SampleContainer::U8,
        ))
    }

    /// `P010`: 4:2:0 semi-planar, 10 bits in the high bits of a 16-bit word.
    pub const fn p010() -> Self {
        Self::P010(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits10,
            SampleContainer::U16,
        ))
    }

    /// `P012`: 4:2:0 semi-planar, 12 bits in a 16-bit word.
    pub const fn p012() -> Self {
        Self::P012(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits12,
            SampleContainer::U16,
        ))
    }

    /// `P016`: 4:2:0 semi-planar, a full 16-bit sample.
    pub const fn p016() -> Self {
        Self::P016(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits16,
            SampleContainer::U16,
        ))
    }

    /// `I010`: 4:2:0 planar, 10 bits in a 16-bit word.
    pub const fn i010() -> Self {
        Self::I010(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits10,
            SampleContainer::U16,
        ))
    }

    /// `I012`: 4:2:0 planar, 12 bits in a 16-bit word.
    pub const fn i012() -> Self {
        Self::I012(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits12,
            SampleContainer::U16,
        ))
    }

    /// `I016`: 4:2:0 planar, a full 16-bit sample.
    pub const fn i016() -> Self {
        Self::I016(SurfaceFormat::yuv(
            Subsampling::C420,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits16,
            SampleContainer::U16,
        ))
    }

    /// `P210`: 4:2:2 semi-planar, 10 bits in a 16-bit word.
    pub const fn p210() -> Self {
        Self::P210(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits10,
            SampleContainer::U16,
        ))
    }

    /// `P212`: 4:2:2 semi-planar, 12 bits in a 16-bit word.
    pub const fn p212() -> Self {
        Self::P212(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits12,
            SampleContainer::U16,
        ))
    }

    /// `P216`: 4:2:2 semi-planar, a full 16-bit sample.
    pub const fn p216() -> Self {
        Self::P216(SurfaceFormat::yuv(
            Subsampling::C422,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits16,
            SampleContainer::U16,
        ))
    }

    /// `P410`: 4:4:4 semi-planar, 10 bits in a 16-bit word.
    pub const fn p410() -> Self {
        Self::P410(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits10,
            SampleContainer::U16,
        ))
    }

    /// `P412`: 4:4:4 semi-planar, 12 bits in a 16-bit word.
    pub const fn p412() -> Self {
        Self::P412(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits12,
            SampleContainer::U16,
        ))
    }

    /// `P416`: 4:4:4 semi-planar, a full 16-bit sample.
    pub const fn p416() -> Self {
        Self::P416(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::SemiPlanar,
            ChromaOrder::CbCr,
            SampleDepth::Bits16,
            SampleContainer::U16,
        ))
    }

    /// `Q410`: 4:4:4 planar, 10 bits in a 16-bit word.
    pub const fn q410() -> Self {
        Self::Q410(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits10,
            SampleContainer::U16,
        ))
    }

    /// `Q416`: 4:4:4 planar, a full 16-bit sample.
    pub const fn q416() -> Self {
        Self::Q416(SurfaceFormat::yuv(
            Subsampling::C444,
            PlaneLayout::Planar,
            ChromaOrder::CbCr,
            SampleDepth::Bits16,
            SampleContainer::U16,
        ))
    }

    /// The pre-constructed shape this kind names.
    pub const fn dimensions(self) -> SurfaceFormat {
        match self {
            Self::Bgra8(format) | Self::Rgba8(format) => format,
            Self::Nv12(format)
            | Self::Nv21(format)
            | Self::I420(format)
            | Self::Yv12(format)
            | Self::Nv16(format)
            | Self::Nv61(format)
            | Self::Yu16(format)
            | Self::Yv16(format)
            | Self::Nv24(format)
            | Self::Yuv444(format)
            | Self::Mono8(format) => format,
            Self::P010(format)
            | Self::P012(format)
            | Self::P016(format)
            | Self::I010(format)
            | Self::I012(format)
            | Self::I016(format)
            | Self::P210(format)
            | Self::P212(format)
            | Self::P216(format)
            | Self::P410(format)
            | Self::P412(format)
            | Self::P416(format)
            | Self::Q410(format)
            | Self::Q416(format) => format,
        }
    }

    /// Whether the colour is YCbCr; see [`SurfaceFormat::is_yuv`].
    pub const fn is_yuv(self) -> bool {
        self.dimensions().is_yuv()
    }

    /// How many planes a buffer of this kind carries; see [`SurfaceFormat::plane_count`].
    pub const fn plane_count(self) -> usize {
        self.dimensions().plane_count()
    }
}

impl std::fmt::Display for SurfaceFormatKind {
    /// The format's conventional name, for diagnostics — a dropped surface says which format it was.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bgra8(_) => "BGRA8",
            Self::Rgba8(_) => "RGBA8",
            Self::Nv12(_) => "NV12",
            Self::Nv21(_) => "NV21",
            Self::I420(_) => "I420",
            Self::Yv12(_) => "YV12",
            Self::Nv16(_) => "NV16",
            Self::Nv61(_) => "NV61",
            Self::Yu16(_) => "YU16",
            Self::Yv16(_) => "YV16",
            Self::Nv24(_) => "NV24",
            Self::Yuv444(_) => "YUV444",
            Self::Mono8(_) => "MONO8",
            Self::P010(_) => "P010",
            Self::P012(_) => "P012",
            Self::P016(_) => "P016",
            Self::I010(_) => "I010",
            Self::I012(_) => "I012",
            Self::I016(_) => "I016",
            Self::P210(_) => "P210",
            Self::P212(_) => "P212",
            Self::P216(_) => "P216",
            Self::P410(_) => "P410",
            Self::P412(_) => "P412",
            Self::P416(_) => "P416",
            Self::Q410(_) => "Q410",
            Self::Q416(_) => "Q416",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_packed_rgb_format_is_one_plane_and_not_yuv() {
        let format = SurfaceFormatKind::bgra8().dimensions();
        assert_eq!(format.model(), ColorModel::Rgb);
        assert_eq!(format.rgb_order(), RgbOrder::Bgr);
        assert_eq!(format.plane_count(), 1);
        assert!(!format.is_yuv());
    }

    #[test]
    fn a_semi_planar_yuv_format_is_two_planes() {
        let format = SurfaceFormatKind::nv12().dimensions();
        assert!(format.is_yuv());
        assert_eq!(format.subsampling(), Subsampling::C420);
        assert_eq!(format.layout(), PlaneLayout::SemiPlanar);
        assert_eq!(format.plane_count(), 2);
        assert_eq!(format.depth(), SampleDepth::Bits8);
        assert_eq!(SurfaceFormatKind::nv12().plane_count(), 2);
    }

    #[test]
    fn a_planar_yuv_format_is_three_planes_bar_mono() {
        assert_eq!(SurfaceFormatKind::i420().plane_count(), 3);
        assert_eq!(SurfaceFormatKind::q416().plane_count(), 3);
        assert_eq!(SurfaceFormatKind::mono8().plane_count(), 1);
    }

    #[test]
    fn a_sixteen_bit_container_carries_ten_bits_in_its_high_bits() {
        let format = SurfaceFormatKind::p010().dimensions();
        assert_eq!(format.depth(), SampleDepth::Bits10);
        assert_eq!(format.container(), SampleContainer::U16);
    }

    #[test]
    fn the_chroma_orders_differ_only_in_which_component_comes_first() {
        let cb_cr = SurfaceFormatKind::nv12().dimensions();
        let cr_cb = SurfaceFormatKind::nv21().dimensions();
        assert_eq!(cb_cr.chroma_order(), ChromaOrder::CbCr);
        assert_eq!(cr_cb.chroma_order(), ChromaOrder::CrCb);
        assert_ne!(cb_cr, cr_cb);
        assert_eq!(cb_cr.subsampling(), cr_cb.subsampling());
        assert_eq!(cb_cr.plane_count(), cr_cb.plane_count());
    }

    #[test]
    fn a_kind_displays_as_its_conventional_name() {
        assert_eq!(SurfaceFormatKind::nv12().to_string(), "NV12");
        assert_eq!(SurfaceFormatKind::p010().to_string(), "P010");
        assert_eq!(SurfaceFormatKind::bgra8().to_string(), "BGRA8");
    }

    /// Two kinds are the same only when they name the same format and carry the same shape.
    #[test]
    fn kinds_compare_by_their_shape() {
        assert_eq!(SurfaceFormatKind::nv12(), SurfaceFormatKind::nv12());
        assert_ne!(SurfaceFormatKind::nv12(), SurfaceFormatKind::nv21());
        assert_ne!(SurfaceFormatKind::nv12(), SurfaceFormatKind::i420());
    }
}
