use anyhow::{Context as _, Result};
use image::{DynamicImage, Frame};
use seahash::SeaHasher;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::hash::{Hash, Hasher};
use std::io::Cursor;

#[allow(missing_docs)]
pub fn decode_static_image(
    bytes: &[u8],
    format: image::ImageFormat,
) -> Result<SmallVec<[Frame; 1]>> {
    let decoder = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .context("creating image decoder")?;
    decode_static_image_from_decoder(decoder)
}

#[allow(missing_docs)]
pub fn decode_static_image_from_decoder(
    mut decoder: impl image::ImageDecoder,
) -> Result<SmallVec<[Frame; 1]>> {
    let orientation = decoder
        .orientation()
        .context("reading decoder's orientation")?;
    let mut image = DynamicImage::from_decoder(decoder).context("decoding image")?;
    image.apply_orientation(orientation);

    let mut data = image.into_rgba8();
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }

    Ok(SmallVec::from_elem(Frame::new(data), 1))
}

/// A clipboard item that should be copied to the clipboard
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardString {
    /// The text content.
    pub text: String,
    /// Optional metadata associated with this clipboard string.
    pub metadata: Option<String>,
}

impl ClipboardString {
    /// Create a new clipboard string with the given text
    pub fn new(text: String) -> Self {
        Self {
            text,
            metadata: None,
        }
    }

    /// Return a new clipboard item with the metadata replaced by the given metadata,
    /// after serializing it as JSON.
    pub fn with_json_metadata<T: Serialize>(mut self, metadata: T) -> Self {
        self.metadata = Some(serde_json::to_string(&metadata).unwrap());
        self
    }

    /// Get the text of the clipboard string
    pub fn text(&self) -> &String {
        &self.text
    }

    /// Get the owned text of the clipboard string
    pub fn into_text(self) -> String {
        self.text
    }

    /// Get the metadata of the clipboard string, formatted as JSON
    pub fn metadata_json<T>(&self) -> Option<T>
    where
        T: for<'a> Deserialize<'a>,
    {
        self.metadata
            .as_ref()
            .and_then(|m| serde_json::from_str(m).ok())
    }

    #[cfg_attr(any(target_os = "linux", target_os = "freebsd"), allow(dead_code))]
    /// Compute a hash of the given text for clipboard change detection.
    pub fn text_hash(text: &str) -> u64 {
        let mut hasher = SeaHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }
}

impl From<String> for ClipboardString {
    fn from(value: String) -> Self {
        Self {
            text: value,
            metadata: None,
        }
    }
}
