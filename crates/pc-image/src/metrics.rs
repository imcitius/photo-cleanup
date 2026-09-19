//! Measurements of a frame's technical quality, taken at native resolution.
//!
//! These exist to answer "which frame of this burst is the keeper", so they
//! are only meaningful *within* a series: sharpness in particular depends on
//! resolution, and comparing a 24 MP frame to a web export tells you nothing.

use image::{DynamicImage, GrayImage};

/// Working height for the gradient pass. Sharpness lives in the high
/// frequencies, so the image is not downscaled — it is subsampled, which
/// keeps neighbouring pixels neighbouring.
const MAX_SAMPLES: usize = 4_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Metrics {
    /// Mean gradient magnitude across the sharpest twentieth of the frame.
    ///
    /// The top slice rather than the whole image, because a portrait at f/1.8
    /// is mostly out-of-focus background by area: a global measure would call
    /// a tack-sharp subject blurry.
    pub sharpness: f32,
    /// Fraction of pixels crushed to black.
    pub clip_low: f32,
    /// Fraction of pixels blown to white.
    pub clip_high: f32,
    /// Entropy of the luminance histogram, in bits. Flat or empty frames
    /// score low.
    pub entropy: f32,
    /// Standard deviation of luminance.
    pub contrast: f32,
}

fn gray_of(img: &DynamicImage) -> GrayImage {
    let g = img.to_luma8();
    let pixels = g.width() as usize * g.height() as usize;
    if pixels <= MAX_SAMPLES {
        return g;
    }
    // Subsample on a lattice rather than resampling: a filtered downscale
    // would smooth away exactly the detail being measured.
    let step = ((pixels as f64 / MAX_SAMPLES as f64).sqrt().ceil() as u32).max(1);
    let (w, h) = (g.width() / step, g.height() / step);
    GrayImage::from_fn(w.max(1), h.max(1), |x, y| {
        *g.get_pixel(
            (x * step).min(g.width() - 1),
            (y * step).min(g.height() - 1),
        )
    })
}

pub fn measure(img: &DynamicImage) -> Metrics {
    let g = gray_of(img);
    let (w, h) = (g.width(), g.height());
    if w < 3 || h < 3 {
        return Metrics::default();
    }
    let raw = g.as_raw();
    let idx = |x: u32, y: u32| raw[(y * w + x) as usize] as f32;

    // ---- histogram, exposure, contrast ----
    let mut hist = [0u32; 256];
    for &p in raw {
        hist[p as usize] += 1;
    }
    let n = raw.len() as f32;
    let clip_low = hist[0..=2].iter().sum::<u32>() as f32 / n;
    let clip_high = hist[253..=255].iter().sum::<u32>() as f32 / n;

    let mut entropy = 0.0f32;
    for &c in &hist {
        if c > 0 {
            let p = c as f32 / n;
            entropy -= p * p.log2();
        }
    }

    let mean = raw.iter().map(|&p| p as f32).sum::<f32>() / n;
    let var = raw.iter().map(|&p| (p as f32 - mean).powi(2)).sum::<f32>() / n;

    // ---- gradient magnitude, Sobel ----
    let mut mags: Vec<f32> = Vec::with_capacity(((w - 2) * (h - 2)) as usize);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = -idx(x - 1, y - 1) - 2.0 * idx(x - 1, y) - idx(x - 1, y + 1)
                + idx(x + 1, y - 1)
                + 2.0 * idx(x + 1, y)
                + idx(x + 1, y + 1);
            let gy = -idx(x - 1, y - 1) - 2.0 * idx(x, y - 1) - idx(x + 1, y - 1)
                + idx(x - 1, y + 1)
                + 2.0 * idx(x, y + 1)
                + idx(x + 1, y + 1);
            mags.push((gx * gx + gy * gy).sqrt());
        }
    }

    let sharpness = if mags.is_empty() {
        0.0
    } else {
        let keep = (mags.len() / 20).max(1);
        let at = mags.len() - keep;
        mags.select_nth_unstable_by(at, |a, b| {
            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
        });
        let top = &mags[at..];
        top.iter().sum::<f32>() / top.len() as f32
    };

    Metrics {
        sharpness,
        clip_low,
        clip_high,
        entropy,
        contrast: var.sqrt(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, Rgb, RgbImage};

    fn checker(w: u32, h: u32, cell: u32) -> DynamicImage {
        let mut img = RgbImage::new(w, h);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let on = ((x / cell) + (y / cell)).is_multiple_of(2);
            let v = if on { 230 } else { 25 };
            *p = Rgb([v, v, v]);
        }
        DynamicImage::ImageRgb8(img)
    }

    /// The same scene through a box blur: fewer high frequencies, same layout.
    fn blurred(img: &DynamicImage, radius: u32) -> DynamicImage {
        DynamicImage::ImageLuma8(image::imageops::blur(&img.to_luma8(), radius as f32))
    }

    #[test]
    fn a_blurred_frame_scores_below_the_sharp_one() {
        let sharp = checker(256, 256, 8);
        let soft = blurred(&sharp, 3);
        let (a, b) = (measure(&sharp).sharpness, measure(&soft).sharpness);
        assert!(a > b * 2.0, "резкий {a:.1} против размытого {b:.1}");
    }

    #[test]
    fn a_sharp_subject_on_a_blurred_background_is_not_called_blurry() {
        // A portrait at f/1.8 is mostly out-of-focus by area. Measuring the
        // whole frame would bury the subject; the top slice keeps it.
        let sharp = checker(256, 256, 6).to_luma8();
        let soft = image::imageops::blur(&sharp, 4.0);
        let mixed = GrayImage::from_fn(256, 256, |x, y| {
            // A small sharp subject in the middle of a soft background.
            if (100..156).contains(&x) && (100..156).contains(&y) {
                *sharp.get_pixel(x, y)
            } else {
                *soft.get_pixel(x, y)
            }
        });
        let all_soft = DynamicImage::ImageLuma8(soft);
        let with_subject = DynamicImage::ImageLuma8(mixed);
        let a = measure(&with_subject).sharpness;
        let b = measure(&all_soft).sharpness;
        assert!(a > b * 1.5, "с резким субъектом {a:.1}, весь мягкий {b:.1}");
    }

    #[test]
    fn clipping_is_measured_at_both_ends() {
        let white = DynamicImage::ImageLuma8(GrayImage::from_pixel(64, 64, Luma([255])));
        let black = DynamicImage::ImageLuma8(GrayImage::from_pixel(64, 64, Luma([0])));
        assert!(measure(&white).clip_high > 0.99);
        assert!(measure(&white).clip_low < 0.01);
        assert!(measure(&black).clip_low > 0.99);
    }

    #[test]
    fn a_flat_frame_has_almost_no_entropy_and_no_contrast() {
        let flat = DynamicImage::ImageLuma8(GrayImage::from_pixel(64, 64, Luma([128])));
        let m = measure(&flat);
        assert!(m.entropy < 0.01, "энтропия {}", m.entropy);
        assert!(m.contrast < 0.01);
        assert!(m.sharpness < 0.01);
    }

    #[test]
    fn a_detailed_frame_has_high_entropy() {
        let m = measure(&checker(128, 128, 4));
        assert!(m.entropy > 0.9, "энтропия {}", m.entropy);
        assert!(m.contrast > 50.0);
    }

    #[test]
    fn a_tiny_image_does_not_panic() {
        for side in [1u32, 2, 3] {
            let img = DynamicImage::ImageLuma8(GrayImage::from_pixel(side, side, Luma([10])));
            let _ = measure(&img);
        }
    }

    #[test]
    fn subsampling_keeps_large_frames_comparable() {
        // A 24 MP frame is subsampled; the ordering it produces must survive.
        let sharp = checker(3000, 2000, 10);
        let soft = blurred(&sharp, 3);
        assert!(measure(&sharp).sharpness > measure(&soft).sharpness);
    }
}
