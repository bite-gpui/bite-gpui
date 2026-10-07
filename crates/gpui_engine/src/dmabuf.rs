//! Linux surface descriptors: the dma-buf a producer hands the renderer.
//!
//! Linux's transport is a dma-buf — an fd plus a DRM fourcc, a modifier, a stride and an offset —
//! the sibling of macOS's `IOSurface` and Windows's DXGI NT handle. The engine owns the descriptor
//! because [`SurfaceSource`](crate::SurfaceSource) names it, exactly as it names `CVPixelBuffer` and
//! `ID3D11ShaderResourceView`; it is plain data (descriptors and layout), so it pulls no GPU driver
//! into the engine.
//!
//! A [`DmaBufHandle`] bundles that descriptor with the layout an importer needs to sample it: the
//! surface size, the pixel [`format`](DmaBufHandle::format), the DRM format
//! [`modifier`](DmaBufHandle::modifier), one [`DmaBufPlane`] per plane (an fd, a byte offset and a row
//! stride), an optional acquire fence, and — for a YCbCr buffer — how to reconstruct
//! [`chroma`](DmaBufHandle::chroma) and which [`color_space`](DmaBufHandle::color_space) the bytes are
//! in. A producer builds one and hands it to the renderer; the renderer duplicates the plane
//! descriptors into the kernel when it imports.
//!
//! # The producer's contract
//!
//! The renderer *consumes* the descriptor; it does not negotiate it, and it cannot repair a buffer
//! that violates one. These are the invariants an importer requires, so a producer must satisfy
//! them:
//!
//! - **Uncompressed under the declared [`modifier`](DmaBufHandle::modifier).** A driver may enable
//!   implicit (CCS) compression for a sampled tiled image, and that state is not carried by the
//!   exported plane(s); an importer then reads compressed bytes as raw pixels.
//! - **Linear across vendors.** A buffer shared between GPUs of different vendors must use
//!   [`DRM_FORMAT_MOD_LINEAR`](DmaBufHandle::LINEAR) — a vendor-private tiled modifier means nothing
//!   to a different vendor.
//! - **A dedicated allocation.** A buffer whose planes are adopted as separate textures (as `NV12`
//!   is) must be allocated dedicated; some drivers refuse the import otherwise.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;

use smallvec::SmallVec;

use crate::SurfaceFormatKind;

/// One plane of a dma-buf: a descriptor, its offset into the buffer, and its row stride.
///
/// The descriptor is shared (`Arc`) so a handle clones without duplicating descriptors: the producer
/// keeps its own reference, and the renderer duplicates in the kernel when it imports.
#[derive(Debug, Clone)]
pub struct DmaBufPlane {
    /// The plane's file descriptor.
    pub fd: Arc<OwnedFd>,
    /// The plane's offset within the buffer, in bytes.
    pub offset: u64,
    /// The plane's row stride, in bytes.
    pub stride: u32,
}

// Identity equality: the same descriptor number and the same layout. `OwnedFd` is neither `Clone`
// nor `Eq`, and a scene compares primitives rather than owning them, so identity is what is meant.
impl PartialEq for DmaBufPlane {
    fn eq(&self, other: &Self) -> bool {
        self.fd.as_raw_fd() == other.fd.as_raw_fd()
            && self.offset == other.offset
            && self.stride == other.stride
    }
}

impl Eq for DmaBufPlane {}

impl DmaBufPlane {
    /// A plane from an owned descriptor, its byte offset within the buffer, and its row stride.
    pub fn new(fd: OwnedFd, offset: u64, stride: u32) -> Self {
        Self {
            fd: Arc::new(fd),
            offset,
            stride,
        }
    }
}

/// How a renderer reconstructs full-resolution chroma from a 4:2:0 buffer's half-resolution plane.
///
/// The producer's hint, not the format's: an `Nv12` buffer is chroma-subsampled either way, and this
/// says what to do about it. [`Bilinear`](Self::Bilinear) is the honest default — it is what the
/// format carries, and it keeps a wrong stride or plane size visible rather than plausible.
/// [`LumaGuided`](Self::LumaGuided) weights the chroma taps by the full-resolution luma, so colour
/// follows the luma edges instead of being low-passed across them. A renderer may ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChromaReconstruction {
    /// Sample the chroma plane bilinearly, as the format carries it.
    #[default]
    Bilinear,
    /// Reconstruct chroma with the full-resolution luma as a guide.
    LumaGuided,
}

/// The colour matrix relating an `Nv12` buffer's luma/chroma to RGB.
///
/// The producer's declaration, not the format's: an `Nv12` buffer carries YCbCr either way, and which
/// matrix turns it back into RGB is a property of the video, not the buffer. A renderer inverts the
/// matrix named here when it converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum YuvMatrix {
    /// BT.601 — `SMPTE 170M` / `BT.470 BG` / `FCC`. The default, and what standard definition is in.
    #[default]
    Bt601,
    /// BT.709 — what high definition is in.
    Bt709,
    /// BT.2020 non-constant-luminance — UHD and wide gamut.
    Bt2020,
}

/// Whether an `Nv12` buffer's bytes are full or limited ("studio") range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum YuvRange {
    /// Full range: luma over `0..255`, chroma over `0..255`. `JPEG`/`PC` range, and the default here.
    #[default]
    Full,
    /// Limited range: luma over `16..235`, chroma over `16..240`. `MPEG`/`TV` range, and what almost
    /// every real stream is in — a full-range buffer read as limited loses contrast, and the reverse
    /// clips.
    Limited,
}

/// The YCbCr a YCbCr ([`is_yuv`](SurfaceFormatKind::is_yuv)) buffer's bytes are in: the matrix and the
/// range.
///
/// A hint, like [`ChromaReconstruction`] and to the same end — it says how to read the colour the
/// format carries. A renderer that does not understand one falls back to the default, which is
/// BT.601 full range: the shape a hand-filled buffer is in, and what the Linux backend assumed before
/// this was declarable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct YuvColorSpace {
    /// The colour matrix.
    pub matrix: YuvMatrix,
    /// The luma/chroma range.
    pub range: YuvRange,
}

impl YuvColorSpace {
    /// BT.601, limited range — what standard-definition video is almost always in.
    pub const BT601_LIMITED: Self = Self {
        matrix: YuvMatrix::Bt601,
        range: YuvRange::Limited,
    };

    /// BT.709, limited range — what high-definition video is almost always in.
    pub const BT709_LIMITED: Self = Self {
        matrix: YuvMatrix::Bt709,
        range: YuvRange::Limited,
    };
}

/// A dma-buf a producer hands the renderer to composite as a surface.
///
/// See the module documentation for the producer's contract — uncompressed, linear across vendors,
/// dedicated when the planes are split.
#[derive(Debug, Clone)]
pub struct DmaBufHandle {
    /// The surface width, in pixels.
    pub width: u32,
    /// The surface height, in pixels.
    pub height: u32,
    /// The pixel layout.
    pub format: SurfaceFormatKind,
    /// The DRM format modifier (tiling/compression layout); [`Self::LINEAR`] for a linear buffer.
    pub modifier: u64,
    /// One plane per [`SurfaceFormatKind::plane_count`].
    pub planes: SmallVec<[DmaBufPlane; 2]>,
    /// An optional `sync_file` fence the renderer waits on before sampling the buffer; a fence that
    /// does not signal in time drops the surface for that frame rather than blocking the frame.
    pub acquire_fence: Option<Arc<OwnedFd>>,
    /// How the renderer should reconstruct chroma; the producer's hint for a YCbCr buffer, and
    /// ignored for an RGB one.
    pub chroma: ChromaReconstruction,
    /// The colour space this buffer's YCbCr bytes are in; ignored for an RGB one, whose bytes are
    /// already RGB.
    pub color_space: YuvColorSpace,
}

impl PartialEq for DmaBufHandle {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.format == other.format
            && self.modifier == other.modifier
            && self.planes == other.planes
            && self.acquire_fence.as_ref().map(|fd| fd.as_raw_fd())
                == other.acquire_fence.as_ref().map(|fd| fd.as_raw_fd())
            && self.chroma == other.chroma
            && self.color_space == other.color_space
    }
}

impl Eq for DmaBufHandle {}

impl DmaBufHandle {
    /// `DRM_FORMAT_MOD_LINEAR`: the only modifier meaningful across GPU vendors.
    pub const LINEAR: u64 = 0;

    /// Build a handle from its planes.
    ///
    /// `planes` is collected, so a caller need not name `SmallVec` to construct a handle (see
    /// [`SurfaceFormatKind::plane_count`] for how many a format needs); `acquire_fence` is the producer's
    /// optional `sync_file`.
    pub fn new(
        width: u32,
        height: u32,
        format: SurfaceFormatKind,
        modifier: u64,
        planes: impl IntoIterator<Item = DmaBufPlane>,
        acquire_fence: Option<OwnedFd>,
    ) -> Self {
        Self {
            width,
            height,
            format,
            modifier,
            planes: planes.into_iter().collect(),
            acquire_fence: acquire_fence.map(Arc::new),
            chroma: ChromaReconstruction::default(),
            color_space: YuvColorSpace::default(),
        }
    }

    /// Ask the renderer to reconstruct this buffer's chroma [`LumaGuided`](ChromaReconstruction::LumaGuided)
    /// rather than [bilinearly](ChromaReconstruction::Bilinear). A hint: a renderer may ignore it.
    pub fn with_chroma(mut self, chroma: ChromaReconstruction) -> Self {
        self.chroma = chroma;
        self
    }

    /// Declare the colour space this buffer's YCbCr bytes are in. A hint: a renderer that does not
    /// understand the matrix or range falls back to its default (BT.601 full range).
    pub fn with_color_space(mut self, color_space: YuvColorSpace) -> Self {
        self.color_space = color_space;
        self
    }

    /// The number of planes the handle carries, which [`SurfaceFormatKind::plane_count`] should agree
    /// with.
    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real descriptor without a GPU: `/dev/null` is an open file, and any open file is a valid
    /// plane descriptor as far as the descriptor type is concerned.
    fn descriptor() -> OwnedFd {
        std::fs::File::open("/dev/null").expect("/dev/null").into()
    }

    #[test]
    fn linear_is_the_zero_modifier() {
        assert_eq!(DmaBufHandle::LINEAR, 0);
    }

    #[test]
    fn new_collects_the_planes_it_is_given() {
        let handle = DmaBufHandle::new(
            4,
            2,
            SurfaceFormatKind::nv12(),
            DmaBufHandle::LINEAR,
            [
                DmaBufPlane::new(descriptor(), 0, 4),
                DmaBufPlane::new(descriptor(), 8, 4),
            ],
            None,
        );
        assert_eq!(handle.plane_count(), 2);
        assert_eq!((handle.width, handle.height), (4, 2));
        assert_eq!(handle.format, SurfaceFormatKind::nv12());
        assert!(handle.acquire_fence.is_none());
    }

    #[test]
    fn new_wraps_the_acquire_fence() {
        let handle = DmaBufHandle::new(
            1,
            1,
            SurfaceFormatKind::rgba8(),
            DmaBufHandle::LINEAR,
            [DmaBufPlane::new(descriptor(), 0, 4)],
            Some(descriptor()),
        );
        assert!(handle.acquire_fence.is_some());
    }

    #[test]
    fn a_clone_shares_the_descriptor_rather_than_duplicating_it() {
        let plane = DmaBufPlane::new(descriptor(), 0, 4);
        let cloned = plane.clone();
        assert!(Arc::ptr_eq(&plane.fd, &cloned.fd));
    }

    #[test]
    fn equality_is_the_descriptor_and_the_layout() {
        let fd = Arc::new(descriptor());
        let plane = DmaBufPlane {
            fd: fd.clone(),
            offset: 0,
            stride: 4,
        };
        let same = DmaBufPlane {
            fd: fd.clone(),
            offset: 0,
            stride: 4,
        };
        // The same buffer at a different offset is a different plane.
        let elsewhere = DmaBufPlane {
            fd: fd.clone(),
            offset: 4,
            stride: 4,
        };
        assert_eq!(plane, same);
        assert_ne!(plane, elsewhere);
    }

    #[test]
    fn a_handle_becomes_a_surface_source() {
        let handle = DmaBufHandle::new(
            1,
            1,
            SurfaceFormatKind::rgba8(),
            DmaBufHandle::LINEAR,
            [DmaBufPlane::new(descriptor(), 0, 4)],
            None,
        );
        let source: crate::SurfaceSource = handle.clone().into();
        assert_eq!(source, crate::SurfaceSource::DmaBuf(handle));
    }

    /// A handle defaults to BT.601 full range — the space a hand-filled buffer is in, and what the
    /// backend assumed before the colour space was declarable.
    #[test]
    fn a_handle_defaults_to_bt601_full_range() {
        let handle = DmaBufHandle::new(
            2,
            2,
            SurfaceFormatKind::nv12(),
            DmaBufHandle::LINEAR,
            [
                DmaBufPlane::new(descriptor(), 0, 2),
                DmaBufPlane::new(descriptor(), 4, 2),
            ],
            None,
        );
        assert_eq!(
            handle.color_space,
            YuvColorSpace {
                matrix: YuvMatrix::Bt601,
                range: YuvRange::Full,
            },
        );
    }

    /// The declared colour space is part of the handle's identity, so a scene that compares handles
    /// does not take two differently-converted buffers for one.
    #[test]
    fn the_colour_space_is_part_of_identity() {
        let handle = DmaBufHandle::new(
            2,
            2,
            SurfaceFormatKind::nv12(),
            DmaBufHandle::LINEAR,
            [
                DmaBufPlane::new(descriptor(), 0, 2),
                DmaBufPlane::new(descriptor(), 4, 2),
            ],
            None,
        );
        let declared = handle.clone().with_color_space(YuvColorSpace::BT709_LIMITED);
        assert_eq!(declared.color_space, YuvColorSpace::BT709_LIMITED);
        assert_ne!(handle, declared);
    }
}
