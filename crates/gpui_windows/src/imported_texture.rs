//! The Direct3D half of the imported-texture path.
//!
//! The engine transports a texture produced outside GPUI as an erased
//! [`ImportedTextureHandle`], because it owns no GPU type
//! (`crates/gpui_engine/src/custom_render.rs`). The payload and the extension that builds one
//! live here, in the crate that names the Direct3D API; the renderer
//! ([`crate::DirectXRenderer`]) is what downcasts it back.

use anyhow::Result;
use gpui_engine::ImportedTextureHandle;
use windows::Win32::Graphics::{
    Direct3D11::{D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, ID3D11Texture2D},
    Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB},
};

/// The Direct3D payload an [`ImportedTextureHandle`] carries.
///
/// The texture is one the producer made on the renderer's own device, which is the whole of the
/// device rule: the renderer makes the view itself, so there is no handle to open and nothing to
/// synchronise. The per-primitive values — bounds, clip, radii, opacity — travel in the scene's
/// custom primitive rather than here.
#[derive(Clone)]
pub struct DirectXImportedTexture {
    /// The texture the imported-texture fragment samples.
    pub(crate) texture: ID3D11Texture2D,
}

// `ImportedTextureHandle`'s `Arc<dyn Any + Send + Sync>` asks for both, and a Direct3D 11 device
// is not thread-safe. GPUI renders on one thread and the handle crosses none, so this is the same
// assertion `gpui_engine::MetalTexture` makes.
unsafe impl Send for DirectXImportedTexture {}
unsafe impl Sync for DirectXImportedTexture {}

/// Builds an [`ImportedTextureHandle`] from a Direct3D texture.
///
/// This is the producer's half of Path A on Windows, and it is public because an application has
/// to be able to call it: the device comes from the window's erased accessor, and the token from
/// here.
///
/// What the device does not settle is *which* device: a texture has to be made on the window's
/// own renderer's, which is the same-device rule 0002 states, and a texture the renderer cannot
/// view fails in `draw_custom` rather than composing the wrong memory.
pub trait DirectXTextureExt {
    /// Wrap this texture as a handle.
    ///
    /// Validates up front, at the boundary, where the failure is a producer's mistake and the
    /// message can name it. What the fragment needs is a texture it can sample at all, and one
    /// whose channel order is the target's — it writes the sample straight through, so an RGBA
    /// texture would composite with red and blue exchanged rather than failing.
    fn to_imported_handle(&self) -> Result<ImportedTextureHandle>;
}

impl DirectXTextureExt for ID3D11Texture2D {
    fn to_imported_handle(&self) -> Result<ImportedTextureHandle> {
        let desc = unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            self.GetDesc(&mut desc);
            desc
        };
        anyhow::ensure!(
            matches!(
                desc.Format,
                DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
            ),
            "an imported texture must be B8G8R8A8, the channel order the Direct3D target is in, \
             but this one is {:?}",
            desc.Format
        );
        anyhow::ensure!(
            desc.BindFlags & D3D11_BIND_SHADER_RESOURCE.0 as u32 != 0,
            "an imported texture must be created with D3D11_BIND_SHADER_RESOURCE"
        );

        Ok(ImportedTextureHandle::new(DirectXImportedTexture {
            texture: self.clone(),
        }))
    }
}
