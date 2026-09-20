//! Measurements of a frame's technical quality, taken at native resolution.
//!
//! These exist to answer "which frame of this burst is the keeper", so they
//! are only meaningful *within* a series: sharpness in particular depends on
//! resolution, and comparing a 24 MP frame to a web export tells you nothing.

use image::{DynamicImage, GenericImageView, GrayImage};

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
    /// Mean chroma. Paper, screenshots of text and scans sit near zero;
    /// photographs almost never do.
    ///
    /// Absolute, so it falls with the light: the same colourful scene at a
    /// third of the exposure measures a third of the chroma. Use `chroma`
    /// to ask whether there is colour in a frame at all.
    pub saturation: f32,
    /// Colour as a share of brightness, over the pixels bright enough to
    /// carry any: zero for a monochrome frame at any exposure, and unmoved
    /// by how dark the photograph is.
    pub chroma: f32,
    /// Distance between the darkest and the brightest of the frame, ignoring
    /// the outermost tenth of a percent at each end.
    ///
    /// What separates an empty frame from a photograph that is mostly empty:
    /// a lens cap has nothing in it, while a moon on a black sky is one small
    /// bright thing on a large dark nothing.
    pub tonal_range: f32,
    /// Fraction of pixels bright enough to be unprinted paper.
    pub white_fraction: f32,
    /// Fraction of pixels at either extreme. Ink on paper is bimodal; a
    /// photograph fills the middle of the histogram.
    pub bimodality: f32,
    /// Mean number of strong horizontal contrast reversals per row, relative
    /// to width. Lines of text produce a great many; a landscape does not.
    pub text_rows: f32,
    /// Fraction of rows that are nearly empty of reversals.
    ///
    /// This is what separates writing from a picket fence or foliage, which
    /// also produce reversals by the hundred: text comes in lines with
    /// quiet gaps between them, while a fence is busy from top to bottom.
    pub text_banding: f32,
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

/// Colour relative to brightness, over the pixels that have enough of it.
///
/// `(max - min) / max` is the saturation of HSV, and unlike plain chroma it
/// does not fall away with the light. Pixels darker than a quarter of the
/// range are skipped: at those levels the difference between the channels is
/// sensor noise and rounding, and averaging it in would report colour in a
/// black frame.
fn chroma_of(img: &DynamicImage) -> f32 {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return 0.0;
    }
    let step = ((w as f64 * h as f64 / 250_000.0).sqrt().ceil() as u32).max(1);
    let (mut sum, mut n) = (0.0f32, 0u32);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let p = img.get_pixel(x, y).0;
            let hi = p[0].max(p[1]).max(p[2]) as f32;
            let lo = p[0].min(p[1]).min(p[2]) as f32;
            if hi >= 16.0 {
                sum += (hi - lo) / hi;
                n += 1;
            }
            x += step;
        }
        y += step;
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// Mean chroma, sampled on a lattice to keep the cost flat.
fn saturation_of(img: &DynamicImage) -> f32 {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return 0.0;
    }
    let step = ((w as f64 * h as f64 / 250_000.0).sqrt().ceil() as u32).max(1);
    let (mut sum, mut n) = (0.0f32, 0u32);
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            // Read straight from the frame: converting twenty-five million
            // pixels to RGB in order to look at a quarter of a million of
            // them costs more than the whole measurement.
            let p = img.get_pixel(x, y).0;
            let hi = p[0].max(p[1]).max(p[2]) as f32;
            let lo = p[0].min(p[1]).min(p[2]) as f32;
            sum += (hi - lo) / 255.0;
            n += 1;
            x += step;
        }
        y += step;
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// Contrast reversals per row, and how unevenly they are distributed.
fn text_rows_of(g: &GrayImage) -> (f32, f32) {
    let (w, h) = (g.width(), g.height());
    if w < 8 || h < 8 {
        return (0.0, 0.0);
    }
    let raw = g.as_raw();
    // A reversal only counts when it is decisive, so film grain and noise
    // do not read as printing.
    const EDGE: i32 = 40;
    let mut densities: Vec<f32> = Vec::new();
    let step = (h / 200).max(1);
    let mut y = 0;
    while y < h {
        let base = (y * w) as usize;
        let mut crossings = 0u32;
        let mut last_sign = 0i32;
        for x in 1..w as usize {
            let d = raw[base + x] as i32 - raw[base + x - 1] as i32;
            if d.abs() < EDGE {
                continue;
            }
            let sign = d.signum();
            if last_sign != 0 && sign != last_sign {
                crossings += 1;
            }
            last_sign = sign;
        }
        densities.push(crossings as f32 / w as f32);
        y += step;
    }
    if densities.is_empty() {
        return (0.0, 0.0);
    }
    let mean = densities.iter().sum::<f32>() / densities.len() as f32;
    let peak = densities.iter().cloned().fold(0.0f32, f32::max);
    let quiet = if peak <= f32::EPSILON {
        0.0
    } else {
        densities.iter().filter(|d| **d < peak * 0.15).count() as f32 / densities.len() as f32
    };
    (mean, quiet)
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

    // The span of the histogram, with a thousandth trimmed off each end so a
    // single hot pixel or a dust speck does not speak for the whole frame.
    let edge = (n * 0.001) as u32;
    let percentile = |from_low: bool| -> f32 {
        let mut seen = 0u32;
        let levels: Box<dyn Iterator<Item = usize>> = if from_low {
            Box::new(0..256)
        } else {
            Box::new((0..256).rev())
        };
        for i in levels {
            seen += hist[i];
            if seen > edge {
                return i as f32;
            }
        }
        0.0
    };
    let tonal_range = (percentile(false) - percentile(true)).max(0.0);

    let (text_rows, text_banding) = text_rows_of(&g);
    let white_fraction = hist[235..=255].iter().sum::<u32>() as f32 / n;
    let bimodality =
        (hist[0..=40].iter().sum::<u32>() + hist[215..=255].iter().sum::<u32>()) as f32 / n;

    Metrics {
        sharpness,
        clip_low,
        clip_high,
        entropy,
        contrast: var.sqrt(),
        saturation: saturation_of(img),
        chroma: chroma_of(img),
        tonal_range,
        white_fraction,
        bimodality,
        text_rows,
        text_banding,
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

    /// Dark bars on a white ground, as printed text reads to a gradient.
    fn printed_page(w: u32, h: u32) -> DynamicImage {
        let mut img = RgbImage::from_pixel(w, h, Rgb([245, 244, 242]));
        for line in 0..(h / 14) {
            let y0 = line * 14 + 4;
            let mut x = 8;
            while x < w - 20 {
                let word = 6 + (x * 7 + line * 3) % 26;
                for yy in y0..(y0 + 5).min(h) {
                    for xx in x..(x + word).min(w) {
                        img.put_pixel(xx, yy, Rgb([28, 26, 24]));
                    }
                }
                x += word + 7;
            }
        }
        DynamicImage::ImageRgb8(img)
    }

    #[test]
    fn a_printed_page_reads_as_ink_on_paper() {
        let m = measure(&printed_page(800, 1100));
        assert!(m.white_fraction > 0.5, "белого {:.2}", m.white_fraction);
        assert!(m.bimodality > 0.85, "бимодальность {:.2}", m.bimodality);
        assert!(m.saturation < 0.05, "насыщенность {:.3}", m.saturation);
        assert!(m.text_rows > 0.01, "строчность {:.4}", m.text_rows);
        assert!(m.text_banding > 0.2, "полосатость {:.2}", m.text_banding);
    }

    #[test]
    fn a_picket_fence_is_busy_everywhere_rather_than_in_lines() {
        // Vertical stripes give reversals on every row, which is exactly the
        // false positive that would drag fences and foliage into "documents".
        let mut img = RgbImage::new(600, 400);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let _ = y;
            let v = if (x / 7).is_multiple_of(2) { 240 } else { 30 };
            *p = Rgb([v, v, v]);
        }
        let m = measure(&DynamicImage::ImageRgb8(img));
        assert!(m.text_rows > 0.05, "строчность {:.3}", m.text_rows);
        assert!(
            m.text_banding < 0.05,
            "забор выглядит как текст: полосатость {:.2}",
            m.text_banding
        );
    }

    #[test]
    fn a_photograph_does_not_read_as_a_document() {
        // Colour, a filled histogram, and few decisive reversals per row.
        let mut img = RgbImage::new(600, 400);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let fx = x as f32 / 600.0;
            let fy = y as f32 / 400.0;
            *p = Rgb([
                (120.0 + 90.0 * (fx * 6.0).sin()) as u8,
                (110.0 + 70.0 * (fy * 5.0).cos()) as u8,
                (90.0 + 60.0 * ((fx + fy) * 4.0).sin()) as u8,
            ]);
        }
        let m = measure(&DynamicImage::ImageRgb8(img));
        assert!(m.saturation > 0.1, "насыщенность {:.3}", m.saturation);
        assert!(m.bimodality < 0.4, "бимодальность {:.2}", m.bimodality);
        assert!(m.text_rows < 0.01, "строчность {:.4}", m.text_rows);
    }

    #[test]
    fn a_grey_wall_is_not_a_document_either() {
        // Unsaturated and bright, but with nothing printed on it.
        let flat = DynamicImage::ImageRgb8(RgbImage::from_pixel(400, 400, Rgb([240, 240, 240])));
        let m = measure(&flat);
        assert!(m.white_fraction > 0.9);
        assert!(m.text_rows < 0.001, "строчность {:.4}", m.text_rows);
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
