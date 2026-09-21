//! Content and perceptual hashes.
//!
//! Three separate questions get three separate answers:
//!
//!  * are these the same bytes            -> `blake3`
//!  * are these the same pixels           -> `pixel_hash`
//!  * do these look like the same picture -> `phash` / `dhash`, then `ssim`
//!
//! The perceptual hashes are filters, never proof. A pair they bring together
//! is only promoted to a duplicate after `ssim` agrees, because a pHash
//! collides happily on blank scans, screenshots and evenly lit walls.

pub mod dct;
pub mod ssim;

pub use ssim::ssim;

use image::{imageops::FilterType, GrayImage};

/// Side of the grid the perceptual hashes are computed on.
pub const HASH_SIDE: u32 = 32;
/// Side of the low-frequency block kept from the DCT: 8x8 gives 64 bits.
const DCT_KEEP: usize = 8;

pub fn blake3(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Hash of the whole frame as it is shown: colour, native resolution, aspect.
///
/// This is the evidence behind the word "copy". `pixel_hash` cannot carry it:
/// that one hashes a grey 128x128 square built for comparing *likeness*, and
/// on the way to it the colour is thrown away, the aspect is squashed and the
/// resolution is gone. A red frame and a green frame of equal brightness hash
/// the same there; so do a photograph and its own downscaled export.
///
/// Here nothing is normalised away. Two files match only when they decode to
/// the same picture, pixel for pixel — which is what the interface promises
/// before it moves anything.
pub fn content_hash(img: &image::DynamicImage) -> [u8; 32] {
    let rgb = img.to_rgb8();
    let mut h = blake3::Hasher::new();
    h.update(b"rgb8");
    h.update(&rgb.width().to_le_bytes());
    h.update(&rgb.height().to_le_bytes());
    h.update(rgb.as_raw());
    *h.finalize().as_bytes()
}

/// The perceptual hash of whichever of the eight turns of this frame reads
/// smallest.
///
/// A photograph rotated a quarter turn is the same photograph, and `phash`
/// says the opposite: the DCT of a turned frame differs in nearly every bit,
/// so a rotated duplicate never meets its original. The eight turns of a
/// square — four rotations, each with its mirror — are the ones that happen
/// in practice: a rewritten orientation tag, an import that turned the frame,
/// a film scanned back to front.
///
/// Rotating the square is index arithmetic, not resampling, so this costs
/// eight small DCTs and no image work worth measuring.
pub fn canonical_phash(gray: &GrayImage) -> u64 {
    let (w, h) = (gray.dimensions().0, gray.dimensions().1);
    let at = |x: u32, y: u32| gray.get_pixel(x, y).0[0];
    /// Where a pixel of a turned frame comes from in the upright one.
    type Turn = fn(u32, u32, u32, u32) -> (u32, u32);
    // One for each of the eight symmetries of a square.
    let turns: [Turn; 8] = [
        |x, y, _, _| (x, y),
        |x, y, w, _| (w - 1 - x, y),
        |x, y, _, h| (x, h - 1 - y),
        |x, y, w, h| (w - 1 - x, h - 1 - y),
        |x, y, _, _| (y, x),
        |x, y, _, h| (y, h - 1 - x),
        |x, y, w, _| (w - 1 - y, x),
        |x, y, w, h| (w - 1 - y, h - 1 - x),
    ];
    turns
        .iter()
        .map(|turn| {
            let turned = GrayImage::from_fn(w, h, |x, y| {
                let (sx, sy) = turn(x, y, w, h);
                image::Luma([at(sx.min(w - 1), sy.min(h - 1))])
            });
            phash(&turned)
        })
        .min()
        .unwrap_or(0)
}

/// Hash of the decoded, orientation-normalised pixels.
///
/// Two files that differ only in metadata — a stripped EXIF, a rewritten
/// container — produce different `blake3` but identical `pixel_hash`.
pub fn pixel_hash(gray: &GrayImage) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(&gray.width().to_le_bytes());
    h.update(&gray.height().to_le_bytes());
    h.update(gray.as_raw());
    *h.finalize().as_bytes()
}

fn to_square(gray: &GrayImage, side: u32) -> Vec<f32> {
    // Lanczos, not triangle: reducing 512px to 32px with a triangle filter
    // undersamples badly, and the aliasing differs with the ratio — so the
    // same photograph at two sizes would hash to different values, which is
    // precisely what a perceptual hash must not do.
    let src = if gray.width() == side && gray.height() == side {
        gray.clone()
    } else {
        image::imageops::resize(gray, side, side, FilterType::Lanczos3)
    };
    src.as_raw().iter().map(|&p| p as f32).collect()
}

/// DCT-based perceptual hash: robust to rescaling and re-encoding, which is
/// exactly the difference between a camera JPEG and an export of it.
pub fn phash(gray: &GrayImage) -> u64 {
    let side = HASH_SIDE as usize;
    let pixels = to_square(gray, HASH_SIDE);
    let coeffs = dct::dct_2d(&pixels, side);

    // The DC term encodes overall brightness, not structure, so it is skipped
    // when choosing the threshold as well as when emitting bits.
    let mut low = Vec::with_capacity(DCT_KEEP * DCT_KEEP);
    for y in 0..DCT_KEEP {
        for x in 0..DCT_KEEP {
            low.push(coeffs[y * side + x]);
        }
    }
    let mut sorted: Vec<f32> = low.iter().skip(1).copied().collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    let mut bits = 0u64;
    for (i, v) in low.iter().enumerate() {
        if *v > median {
            bits |= 1 << i;
        }
    }
    bits
}

/// Gradient hash: cheap, and fails differently from `phash`, so agreement
/// between the two is worth more than either alone.
pub fn dhash(gray: &GrayImage) -> u64 {
    let small = image::imageops::resize(gray, 9, 8, FilterType::Lanczos3);
    let mut bits = 0u64;
    let mut i = 0;
    for y in 0..8u32 {
        for x in 0..8u32 {
            let l = small.get_pixel(x, y).0[0];
            let r = small.get_pixel(x + 1, y).0[0];
            if l > r {
                bits |= 1 << i;
            }
            i += 1;
        }
    }
    bits
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Perceptual hashes of the centre and the four quadrants.
///
/// A crop of a photograph keeps one of these regions nearly intact even when
/// the whole-frame hash has moved far away — geometric coverage for free,
/// before any model is involved.
pub fn crop_hashes(gray: &GrayImage) -> [u64; 5] {
    let (w, h) = (gray.width(), gray.height());
    let (hw, hh) = (w / 2, h / 2);
    let regions = [
        (w / 4, h / 4, hw, hh), // centre
        (0, 0, hw, hh),
        (hw, 0, w - hw, hh),
        (0, hh, hw, h - hh),
        (hw, hh, w - hw, h - hh),
    ];
    let mut out = [0u64; 5];
    for (i, (x, y, rw, rh)) in regions.iter().enumerate() {
        let crop = image::imageops::crop_imm(gray, *x, *y, *rw, *rh).to_image();
        out[i] = phash(&crop);
    }
    out
}

/// Whether any region of one image matches any region of the other.
/// Returns the closest distance found.
pub fn crop_distance(a: &[u64; 5], b: &[u64; 5]) -> u32 {
    let mut best = 64;
    for x in a {
        for y in b {
            best = best.min(hamming(*x, *y));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma};

    /// A scene built from low-frequency waves in normalised coordinates, so
    /// it looks the same at any resolution — like a photograph, and unlike
    /// pixel noise, which aliases differently at every scale.
    fn scene(w: u32, h: u32, seed: u32) -> GrayImage {
        let s = seed as f32;
        GrayImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let tau = std::f32::consts::TAU;
            let a = ((fx * (2.0 + s * 0.7) + s * 0.31) * tau).sin();
            let b = ((fy * (1.5 + s * 0.5) + s * 0.93) * tau).cos();
            let c = (((fx + fy) * (3.0 + s * 0.4)) * tau).sin() * 0.5;
            Luma([(128.0 + (a + b + c) * 38.0).clamp(0.0, 255.0) as u8])
        })
    }

    /// What the pipeline actually hashes: every image is normalised to one
    /// grayscale square before any hash is taken.
    fn normalise(img: &GrayImage) -> GrayImage {
        image::imageops::resize(img, 128, 128, FilterType::Lanczos3)
    }

    #[test]
    fn identical_pixels_hash_identically() {
        let a = scene(64, 64, 1);
        assert_eq!(pixel_hash(&a), pixel_hash(&a.clone()));
        assert_eq!(phash(&a), phash(&a.clone()));
    }

    #[test]
    fn pixel_hash_separates_different_pictures() {
        assert_ne!(pixel_hash(&scene(64, 64, 1)), pixel_hash(&scene(64, 64, 2)));
    }

    #[test]
    fn pixel_hash_ignores_nothing_it_should_not() {
        // One pixel apart is a different image as far as pixel identity goes.
        let a = scene(32, 32, 1);
        let mut b = a.clone();
        b.put_pixel(0, 0, Luma([a.get_pixel(0, 0).0[0].wrapping_add(1)]));
        assert_ne!(pixel_hash(&a), pixel_hash(&b));
    }

    #[test]
    fn phash_survives_the_rescale_that_defines_a_t2_duplicate() {
        // The case this exists for: a 6000px original and a 1200px export.
        let big = scene(1536, 1536, 3);
        let small = image::imageops::resize(&big, 300, 300, FilterType::Lanczos3);
        let d = hamming(phash(&normalise(&big)), phash(&normalise(&small)));
        assert!(
            d <= 4,
            "расстояние {d} между кадром и его уменьшенной копией"
        );
    }

    #[test]
    fn phash_survives_a_direct_rescale_without_normalising_first() {
        let big = scene(512, 512, 4);
        let small = image::imageops::resize(&big, 128, 128, FilterType::Lanczos3);
        let d = hamming(phash(&big), phash(&small));
        assert!(d <= 6, "расстояние {d} после прямого уменьшения");
    }

    #[test]
    fn phash_separates_unrelated_pictures() {
        let d = hamming(
            phash(&normalise(&scene(256, 256, 1))),
            phash(&normalise(&scene(256, 256, 9))),
        );
        assert!(d >= 10, "непохожие кадры разошлись всего на {d}");
    }

    #[test]
    fn a_flat_image_is_not_mistaken_for_structure() {
        // Blank scans and black frames must not silently agree with photos.
        let blank = GrayImage::from_pixel(64, 64, Luma([255]));
        let photo = scene(64, 64, 5);
        assert!(hamming(phash(&blank), phash(&photo)) > 0);
    }

    #[test]
    fn dhash_agrees_with_itself_and_differs_across_scenes() {
        let a = normalise(&scene(96, 96, 7));
        assert_eq!(dhash(&a), dhash(&a.clone()));
        let d = hamming(dhash(&a), dhash(&normalise(&scene(96, 96, 2))));
        assert!(d >= 8, "разные кадры разошлись по dhash всего на {d}");
    }

    #[test]
    fn dhash_and_phash_fail_differently() {
        // Their disagreement is the point: agreement between two hashes that
        // break in different ways is worth more than either alone.
        let a = normalise(&scene(128, 128, 1));
        let b = normalise(&scene(128, 128, 6));
        assert!(hamming(phash(&a), phash(&b)) > 0);
        assert!(hamming(dhash(&a), dhash(&b)) > 0);
    }

    #[test]
    fn a_crop_is_caught_by_the_regional_hashes() {
        let full = normalise(&scene(512, 512, 11));
        // Keep the top-left quadrant, as a crop would.
        let cropped = image::imageops::crop_imm(&full, 0, 0, 64, 64).to_image();
        let whole = hamming(phash(&full), phash(&cropped));
        let regional = crop_distance(&crop_hashes(&full), &crop_hashes(&cropped));
        assert!(
            regional < whole,
            "региональные хеши ({regional}) не лучше общего ({whole})"
        );
    }

    #[test]
    fn crop_distance_is_zero_for_a_picture_against_itself() {
        let a = normalise(&scene(256, 256, 13));
        assert_eq!(crop_distance(&crop_hashes(&a), &crop_hashes(&a)), 0);
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use image::{DynamicImage, Rgb, RgbImage};

    fn flat(w: u32, h: u32, colour: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(w, h, Rgb(colour)))
    }

    /// The fault this hash exists to fix: red and green of equal brightness
    /// are one grey square and two different photographs.
    #[test]
    fn colour_is_not_thrown_away() {
        let red = flat(64, 48, [220, 20, 20]);
        let green = flat(64, 48, [20, 220, 20]);
        assert_ne!(
            pixel_hash(&red.to_luma8()),
            [0; 32],
            "проверка бессмысленна, если хеш пуст"
        );
        assert_ne!(content_hash(&red), content_hash(&green));
    }

    #[test]
    fn a_smaller_copy_is_not_the_same_content() {
        let big = flat(400, 300, [10, 120, 200]);
        let small = big.resize_exact(200, 150, image::imageops::FilterType::Triangle);
        assert_ne!(content_hash(&big), content_hash(&small));
    }

    #[test]
    fn the_same_pixels_in_another_container_still_match() {
        // A scan held as BMP and as uncompressed TIFF decodes identically;
        // the bytes differ, the picture does not.
        let a = flat(120, 90, [7, 90, 30]);
        let b = a.clone();
        assert_eq!(content_hash(&a), content_hash(&b));
    }

    fn scene() -> GrayImage {
        GrayImage::from_fn(128, 128, |x, y| {
            let v = ((x * 3 + y * 7) % 200) as u8 + (x / 16) as u8;
            image::Luma([v])
        })
    }

    #[test]
    fn a_quarter_turn_is_the_same_photograph() {
        let upright = scene();
        let turned = image::imageops::rotate90(&upright);
        let mirrored = image::imageops::flip_horizontal(&upright);

        // The plain hash sees strangers; that is the whole problem.
        assert!(hamming(phash(&upright), phash(&turned)) > 10);

        assert_eq!(canonical_phash(&upright), canonical_phash(&turned));
        assert_eq!(canonical_phash(&upright), canonical_phash(&mirrored));
        assert_eq!(
            canonical_phash(&upright),
            canonical_phash(&image::imageops::rotate180(&upright))
        );
    }

    #[test]
    fn two_different_scenes_still_read_apart() {
        let a = scene();
        let b = GrayImage::from_fn(128, 128, |x, y| {
            image::Luma([((x * 11 + y * 2) % 255) as u8])
        });
        assert!(hamming(canonical_phash(&a), canonical_phash(&b)) > 8);
    }
}
