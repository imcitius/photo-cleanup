//! Structural similarity, the verification step behind every perceptual match.
//!
//! A pHash pair is a candidate; SSIM is what promotes it to a duplicate. It is
//! what stops two different receipts, two screenshots of the same app or two
//! blank scans from being grouped together and half of them thrown away.

use image::{imageops::FilterType, GrayImage};

const WINDOW: u32 = 8;
const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);

/// Mean SSIM over 8x8 windows, in `0.0..=1.0`.
///
/// The images are compared at a common size: a downscaled copy of a photo is
/// meant to score high against its original, which is the whole point.
pub fn ssim(a: &GrayImage, b: &GrayImage) -> f64 {
    let w = a.width().min(b.width()).max(WINDOW);
    let h = a.height().min(b.height()).max(WINDOW);

    let fit = |img: &GrayImage| -> GrayImage {
        if img.width() == w && img.height() == h {
            img.clone()
        } else {
            image::imageops::resize(img, w, h, FilterType::Triangle)
        }
    };
    let (a, b) = (fit(a), fit(b));

    let mut total = 0.0;
    let mut windows = 0u32;

    for wy in (0..h.saturating_sub(WINDOW - 1)).step_by(WINDOW as usize) {
        for wx in (0..w.saturating_sub(WINDOW - 1)).step_by(WINDOW as usize) {
            let n = (WINDOW * WINDOW) as f64;
            let (mut sa, mut sb) = (0.0, 0.0);
            for y in 0..WINDOW {
                for x in 0..WINDOW {
                    sa += a.get_pixel(wx + x, wy + y).0[0] as f64;
                    sb += b.get_pixel(wx + x, wy + y).0[0] as f64;
                }
            }
            let (ma, mb) = (sa / n, sb / n);

            let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
            for y in 0..WINDOW {
                for x in 0..WINDOW {
                    let da = a.get_pixel(wx + x, wy + y).0[0] as f64 - ma;
                    let db = b.get_pixel(wx + x, wy + y).0[0] as f64 - mb;
                    va += da * da;
                    vb += db * db;
                    cov += da * db;
                }
            }
            va /= n - 1.0;
            vb /= n - 1.0;
            cov /= n - 1.0;

            let num = (2.0 * ma * mb + C1) * (2.0 * cov + C2);
            let den = (ma * ma + mb * mb + C1) * (va + vb + C2);
            total += if den == 0.0 { 1.0 } else { num / den };
            windows += 1;
        }
    }

    if windows == 0 {
        return 0.0;
    }
    (total / windows as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    fn scene(w: u32, h: u32, seed: u32) -> GrayImage {
        let s = seed as f32;
        GrayImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let tau = std::f32::consts::TAU;
            let a = ((fx * (2.0 + s * 0.7) + s * 0.31) * tau).sin();
            let b = ((fy * (1.5 + s * 0.5) + s * 0.93) * tau).cos();
            Luma([(128.0 + (a + b) * 55.0).clamp(0.0, 255.0) as u8])
        })
    }

    #[test]
    fn an_image_matches_itself_exactly() {
        let a = scene(64, 64, 1);
        assert!((ssim(&a, &a) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_downscaled_copy_still_scores_high() {
        let big = scene(256, 256, 2);
        let small = image::imageops::resize(&big, 96, 96, FilterType::Lanczos3);
        let s = ssim(&big, &small);
        assert!(s > 0.7, "SSIM {s} для уменьшенной копии слишком низок");
    }

    #[test]
    fn different_scenes_score_low() {
        let s = ssim(&scene(128, 128, 1), &scene(128, 128, 9));
        assert!(s < 0.6, "разные кадры получили SSIM {s}");
    }

    #[test]
    fn a_recompressed_copy_stays_above_the_duplicate_threshold() {
        // Simulates the JPEG-quality difference between an original and a
        // re-saved copy: small per-pixel noise, same structure.
        let a = scene(128, 128, 3);
        let b = GrayImage::from_fn(128, 128, |x, y| {
            let v = a.get_pixel(x, y).0[0] as i32 + if (x + y) % 3 == 0 { 4 } else { -3 };
            Luma([v.clamp(0, 255) as u8])
        });
        let s = ssim(&a, &b);
        assert!(s > 0.85, "SSIM {s} для перекодированной копии");
    }

    #[test]
    fn two_blank_images_are_not_compared_by_structure_alone() {
        // Both flat: SSIM is high, which is why blankness is screened out
        // separately rather than relied upon here.
        let a = GrayImage::from_pixel(32, 32, Luma([255]));
        let b = GrayImage::from_pixel(32, 32, Luma([254]));
        assert!(ssim(&a, &b) > 0.9);
    }

    #[test]
    fn mismatched_sizes_are_compared_at_a_common_size() {
        let a = scene(200, 200, 4);
        let b = image::imageops::resize(&a, 50, 50, FilterType::Lanczos3);
        assert!(ssim(&a, &b) > 0.6);
    }
}
