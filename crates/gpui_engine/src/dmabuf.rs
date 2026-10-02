//! Linux surface descriptors: the dma-buf a producer hands the renderer.
//!
//! Linux's transport is a dma-buf — an fd plus a DRM fourcc, a modifier, a stride and an offset —
//! the sibling of macOS's `IOSurface` and Windows's DXGI NT handle. The engine owns the descriptor
//! because [`SurfaceSource`](crate::SurfaceSource) names it, exactly as it names `CVPixelBuffer` and
//! `ID3D11ShaderResourceView`; it is plain data (descriptors and layout), so it pulls no GPU driver
//! into the engine.
//!
//! # The producer's contract
//!
//! These are the invariants the P3 probe measured on real hardware
//! (`bite-gpui-project/decisions/linux-dmabuf-probe.md`). The renderer *consumes* the descriptor; it
//! does not negotiate it, and it cannot repair a buffer that violates one:
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

/// The pixel layout of a dma-buf: the formats the Linux surface arm consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaBufFormat {
    /// A single `BGRA8` plane — the common desktop format.
    Bgra8,
    /// A single `RGBA8` plane.
    Rgba8,
    /// A two-plane `NV12`: full-resolution luma, then half-resolution interleaved chroma.
    Nv12,
}

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

/// A dma-buf a producer hands the renderer to composite as a surface.
///
/// See the [module docs](self) for the producer's contract — uncompressed, linear across vendors,
/// dedicated when the planes are split.
#[derive(Debug, Clone)]
pub struct DmaBufHandle {
    /// The surface width, in pixels.
    pub width: u32,
    /// The surface height, in pixels.
    pub height: u32,
    /// The pixel layout.
    pub format: DmaBufFormat,
    /// The DRM format modifier (tiling/compression layout); [`Self::LINEAR`] for a linear buffer.
    pub modifier: u64,
    /// One plane per [`DmaBufFormat`]: one for `Bgra8`/`Rgba8`, two for `Nv12`.
    pub planes: SmallVec<[DmaBufPlane; 2]>,
    /// An optional `sync_file` fence the renderer waits on before sampling the buffer.
    pub acquire_fence: Option<Arc<OwnedFd>>,
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
    }
}

impl Eq for DmaBufHandle {}

impl DmaBufHandle {
    /// `DRM_FORMAT_MOD_LINEAR`: the only modifier meaningful across GPU vendors.
    pub const LINEAR: u64 = 0;

    /// Build a handle from its planes.
    ///
    /// `planes` is collected (one for `Bgra8`/`Rgba8`, two for `Nv12`), so a caller need not name
    /// `SmallVec` to construct a handle; `acquire_fence` is the producer's optional `sync_file`.
    pub fn new(
        width: u32,
        height: u32,
        format: DmaBufFormat,
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
        }
    }

    /// The number of planes the handle carries: one for `Bgra8`/`Rgba8`, two for `Nv12`.
    pub fn plane_count(&self) -> usize {
        self.planes.len()
    }
}
