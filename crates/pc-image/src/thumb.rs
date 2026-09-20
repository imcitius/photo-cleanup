//! The normalised thumbnail every later stage works from.
//!
//! Orientation is applied here, once. Two files whose pixels match but whose
//! EXIF orientation differs must hash identically, and every consumer
//! downstream would otherwise have to remember to rotate.

use anyhow::{Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, GrayImage};

/// Long edge of the cached thumbnail. Large enough for a 224px model input,
/// an SSIM comparison and a legible grid cell.
pub const THUMB_SIZE: u32 = 384;

/// An embedded preview smaller than this is not worth preferring over a full
/// decode: Sony writes a 160x120 thumbnail next to the large preview.
pub const MIN_PREVIEW_PIXELS: u64 = 256 * 256;

/// Intermediate size the full frame is reduced to before the final resamples.
///
/// A 25 MP frame reaches a 384 px thumbnail through a Lanczos pass over
/// twenty-five million pixels, and then a second pass over the same
/// twenty-five million for the hashing square. One cheap area-average down to
/// this box first makes both passes trivial, and at twice the thumbnail's
/// long edge there is nothing left for Lanczos to recover anyway.
const PRESCALE: u32 = THUMB_SIZE * 2;

/// Side of the square grayscale image kept for hashing.
///
/// The whole-frame hashes only need 32x32, but the regional hashes that catch
/// crops work on quarters of this image, so it is kept larger. At 16 KiB per
/// image it costs nothing to carry.
pub const GRAY_SIDE: u32 = 128;

#[derive(Debug, Clone)]
pub struct Thumbnail {
    /// JPEG bytes for the cache and the UI.
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Grayscale square used by the perceptual hashes.
    pub gray: GrayImage,
}

pub fn decode(bytes: &[u8]) -> Result<DynamicImage> {
    image::load_from_memory(bytes).context(pc_core::tr!(
        "не декодировать изображение",
        "cannot decode the image"
    ))
}

/// Undo the EXIF orientation so stored pixels are upright.
fn apply_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// The frame this thumbnail is built from, reduced once so the two resamples
/// below cost nothing.
fn prescaled(img: DynamicImage) -> DynamicImage {
    if img.width().max(img.height()) <= PRESCALE {
        img
    } else {
        img.thumbnail(PRESCALE, PRESCALE)
    }
}

/// Build the cached thumbnail and hashing square.
///
/// Orientation is applied *after* the reduction rather than before it. EXIF
/// only ever records quarter turns and flips, which commute with a resize
/// into a square box, so the result is the same image — reached without
/// rotating twenty-five million pixels on the way to throwing them away.
pub fn make(img: DynamicImage, orientation: u16) -> Thumbnail {
    let base = prescaled(img);
    let small = apply_orientation(
        base.resize(THUMB_SIZE, THUMB_SIZE, FilterType::Lanczos3),
        orientation,
    );

    let mut jpeg = Vec::new();
    let rgb = small.to_rgb8();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85);
    let _ = enc.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    );

    // Squashed to a square on purpose: aspect ratio is compared separately,
    // and a fixed grid keeps hashes comparable across crops of one scene.
    let gray = apply_orientation(
        base.resize_exact(GRAY_SIDE, GRAY_SIDE, FilterType::Triangle),
        orientation,
    )
    .to_luma8();

    Thumbnail {
        jpeg,
        width: small.width(),
        height: small.height(),
        gray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn gradient(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x * 255 / w.max(1)) as u8, (y * 255 / h.max(1)) as u8, 40]);
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn thumbnail_fits_the_box_and_keeps_aspect() {
        let t = make(gradient(2000, 1000), 1);
        assert_eq!(t.width, THUMB_SIZE);
        assert_eq!(t.height, THUMB_SIZE / 2);
        assert_eq!(t.gray.width(), GRAY_SIDE);
        assert!(!t.jpeg.is_empty());
    }

    #[test]
    fn orientation_is_applied_so_rotated_copies_agree() {
        let upright = gradient(400, 200);
        // The same photograph stored rotated, with EXIF saying so.
        let stored = upright.rotate270();
        let a = make(upright, 1);
        let b = make(stored, 6);
        assert_eq!(a.gray.dimensions(), b.gray.dimensions());
        let diff: u32 = a
            .gray
            .pixels()
            .zip(b.gray.pixels())
            .map(|(x, y)| x.0[0].abs_diff(y.0[0]) as u32)
            .sum();
        let avg = diff / (GRAY_SIDE * GRAY_SIDE);
        assert!(avg < 8, "средняя разница {avg} слишком велика");
    }

    #[test]
    fn the_encoded_thumbnail_can_be_read_back() {
        let t = make(gradient(800, 600), 1);
        let back = decode(&t.jpeg).unwrap();
        assert_eq!(back.width(), t.width);
    }
}
