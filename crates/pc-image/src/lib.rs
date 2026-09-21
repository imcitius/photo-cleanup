//! Reading images cheaply: what they are, what is inside them, and a small
//! normalised thumbnail that every later stage works from.

pub mod jpeg;
pub mod meta;
pub mod metrics;
pub mod read;
pub mod sniff;
pub mod thumb;
pub mod tiff;

pub use meta::{ImageMeta, Provenance};
pub use metrics::Metrics;
pub use read::{read_for_probe, Read1};
pub use sniff::{sniff, Container};
pub use thumb::{Thumbnail, GRAY_SIDE, THUMB_SIZE};

use anyhow::{bail, Result};
use std::path::Path;

/// How many bytes of the head are enough to sniff, read EXIF, and index a
/// TIFF container. Raw files keep their directories near the front.
pub const HEAD_BYTES: usize = 256 * 1024;

/// What one pass over a file produces.
#[derive(Debug, Clone)]
pub struct Probe {
    pub container: Container,
    /// True when the extension disagreed with the magic bytes.
    pub extension_lied: bool,
    pub width: u32,
    pub height: u32,
    pub meta: ImageMeta,
    /// Where the pixels used for hashing came from.
    pub source: PixelSource,
    pub thumb: Thumbnail,
    /// Technical quality, measured on the decoded frame before downscaling.
    pub metrics: Metrics,
    /// The whole frame as it is shown, hashed at its own size and in colour.
    /// This is what "the same picture" is allowed to mean.
    pub content_hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelSource {
    /// Decoded from the file itself.
    Full,
    /// Lifted from a JPEG preview indexed inside a raw container.
    EmbeddedPreview { width: u32, height: u32 },
}

impl PixelSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::EmbeddedPreview { .. } => "preview",
        }
    }
}

/// Read one image from bytes already in hand.
pub fn probe(path: &Path, bytes: &[u8], name_hint: &str) -> Result<Probe> {
    let preview = sniff::sniff(bytes)
        .is_indexed()
        .then(|| tiff::largest_preview(bytes))
        .flatten()
        .filter(|p| p.pixels() >= thumb::MIN_PREVIEW_PIXELS)
        .map(|p| bytes[p.offset..p.offset + p.len].to_vec());
    probe_parts(path, bytes, preview.as_deref(), name_hint)
}

/// Identify an image, read its metadata, and render the thumbnail that
/// hashing and the UI both work from.
///
/// `head` carries the front of the file, which is where the container magic,
/// the EXIF and the TIFF directories all live — and, for anything that is not
/// an indexed container, the whole file. `preview` is the embedded JPEG a raw
/// container pointed at, already fetched by the caller so the sensor data is
/// never read, let alone decoded.
pub fn probe_parts(
    path: &Path,
    head: &[u8],
    preview: Option<&[u8]>,
    name_hint: &str,
) -> Result<Probe> {
    let container = sniff::sniff(head);
    if !container.is_image() {
        bail!(
            "{}",
            pc_core::tf!("не изображение: {0}", "not an image: {0}", path.display())
        );
    }
    let extension_lied = sniff::hint_from_extension(name_hint).is_some_and(|h| h != container);
    let meta = meta::read(head, container);

    let (pixels, source, width, height) = match preview {
        Some(bytes) => {
            let img = thumb::decode(bytes)?;
            let (pw, ph) = (img.width(), img.height());
            // The raw frame's own dimensions describe the photograph; the
            // preview only describes the proxy we happened to decode.
            let (w, h) = meta.raw_dimensions.unwrap_or((pw, ph));
            (
                img,
                PixelSource::EmbeddedPreview {
                    width: pw,
                    height: ph,
                },
                w,
                h,
            )
        }
        None => {
            let img = thumb::decode(head)?;
            let (w, h) = (img.width(), img.height());
            (img, PixelSource::Full, w, h)
        }
    };

    let metrics = metrics::measure(&pixels);
    // Hashed the way the frame is shown, so a rewritten orientation tag is a
    // different picture here and the same one to the perceptual side, which
    // compares turns.
    let content_hash = if meta.orientation <= 1 {
        // Nothing to turn — and a full frame is tens of megabytes, so copying
        // it just to hash it would double what every worker holds.
        pc_hash::content_hash(&pixels)
    } else {
        pc_hash::content_hash(&thumb::apply_orientation(pixels.clone(), meta.orientation))
    };
    let thumb = thumb::make(pixels, meta.orientation);
    Ok(Probe {
        container,
        extension_lied,
        width,
        height,
        meta,
        source,
        thumb,
        metrics,
        content_hash,
    })
}
