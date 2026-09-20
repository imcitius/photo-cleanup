//! Perceptual candidates and their verification.
//!
//! Hashes propose, SSIM disposes. A pHash pair is only ever a candidate: the
//! hash collides cheerfully on blank scans, evenly lit walls, and two
//! screenshots of the same application with different content. Promoting a
//! pair without looking at the pixels is how a tool like this deletes
//! somebody's documents.

use image::GrayImage;
use pc_core::ThumbStore;
use pc_db::FileInfo;
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct Params {
    /// Hamming distance on the whole-frame hash that still counts as a lead.
    pub phash_max: u32,
    /// Distance on the regional hashes, which is how crops are caught.
    pub crop_max: u32,
    /// Structural similarity a pair must reach to be called the same picture.
    pub ssim_min: f64,
    /// The bar for a pair that is the same size and the same format, where
    /// nothing but the pixels distinguishes a re-encode from a second frame.
    pub ssim_min_same_shape: f64,
    /// Below this variance an image carries no structure to compare, and any
    /// similarity score is meaningless.
    pub min_variance: f64,
    /// Guards against a pathological file dragging in thousands of leads.
    pub max_candidates_per_file: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            phash_max: 10,
            crop_max: 6,
            ssim_min: 0.90,
            ssim_min_same_shape: 0.97,
            min_variance: 120.0,
            max_candidates_per_file: 64,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Candidate {
    pub a: usize,
    pub b: usize,
    pub phash_distance: u32,
    pub via_crop: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Verified {
    pub a: usize,
    pub b: usize,
    pub ssim: f64,
    pub phash_distance: u32,
    pub via_crop: bool,
}

/// All pairs worth looking at, by brute force.
///
/// At this corpus size an exact sweep is faster than building an approximate
/// index: 50k files is 1.25 billion 64-bit comparisons, which a popcount
/// loop clears in a second or two, and the answer is exact.
pub fn candidates(files: &[FileInfo], p: &Params) -> Vec<Candidate> {
    candidates_controlled(files, p, &pc_core::work::Control::default())
}
pub fn candidates_controlled(
    files: &[FileInfo],
    p: &Params,
    control: &pc_core::work::Control,
) -> Vec<Candidate> {
    let out = Mutex::new(Vec::new());
    (0..files.len()).into_par_iter().for_each(|i| {
        if control.check().is_err() {
            return;
        }
        let mut local = Vec::new();
        let a = &files[i];
        for (j, b) in files.iter().enumerate().skip(i + 1) {
            let d = pc_hash::hamming(a.phash, b.phash);
            if d <= p.phash_max {
                local.push(Candidate {
                    a: i,
                    b: j,
                    phash_distance: d,
                    via_crop: false,
                });
                continue;
            }
            // A crop moves the whole-frame hash a long way while leaving one
            // region nearly intact.
            let cd = pc_hash::crop_distance(&a.crops, &b.crops);
            if cd <= p.crop_max {
                local.push(Candidate {
                    a: i,
                    b: j,
                    phash_distance: d,
                    via_crop: true,
                });
            }
        }
        control.advance(0, None);
        if local.len() > p.max_candidates_per_file {
            local.sort_by_key(|c| c.phash_distance);
            local.truncate(p.max_candidates_per_file);
        }
        if !local.is_empty() {
            out.lock().unwrap().extend(local);
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|c| (c.a, c.b));
    v
}

fn variance(g: &GrayImage) -> f64 {
    let n = g.as_raw().len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mean = g.as_raw().iter().map(|&v| v as f64).sum::<f64>() / n;
    g.as_raw()
        .iter()
        .map(|&v| {
            let d = v as f64 - mean;
            d * d
        })
        .sum::<f64>()
        / n
}

/// Thumbnails, decoded once and reused across every pair they appear in.
struct Grays<'a> {
    store: &'a ThumbStore,
    cache: HashMap<usize, Option<GrayImage>>,
}

impl<'a> Grays<'a> {
    fn new(store: &'a ThumbStore) -> Self {
        Self {
            store,
            cache: HashMap::new(),
        }
    }

    fn get(&mut self, idx: usize, files: &[FileInfo]) -> Option<&GrayImage> {
        self.cache
            .entry(idx)
            .or_insert_with(|| {
                let key = files[idx].thumb_key.as_deref()?;
                let bytes = self.store.get(key)?;
                image::load_from_memory(&bytes).ok().map(|i| i.to_luma8())
            })
            .as_ref()
    }
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub verified: Vec<Verified>,
    pub rejected_by_ssim: u64,
    pub rejected_as_blank: u64,
    pub rejected_as_series: u64,
    pub thumbnails_missing: u64,
}

/// Whether one of these files could plausibly have been made from the other.
///
/// Looking alike is not enough. A tripod sequence of one scene scores 0.86 to
/// 0.92 frame to frame, which is comfortably inside any threshold loose
/// enough to catch a re-encode — so similarity alone would collapse a burst
/// into a single "duplicate" and offer to throw most of it away.
///
/// A derivative differs from its parent in some concrete way: it was resized,
/// or converted to another format, or re-encoded from the very same pixels.
/// Absent any of those, two similar frames are two photographs.
pub fn derivation_plausible(a: &FileInfo, b: &FileInfo, ssim: f64, p: &Params) -> bool {
    // Two raw frames are two shutter presses. A raw file is never generated
    // from another raw file, and identical raws are caught exactly by hash.
    if a.is_raw() && b.is_raw() {
        return false;
    }

    let resized = a.pixels() != b.pixels();
    let reformatted = a.container != b.container;
    if resized || reformatted {
        return ssim >= p.ssim_min;
    }

    // Same geometry, same format. Two frames of a sequence live here too, so
    // the bar is a re-encode of identical content, not a resemblance.
    let same_moment = match (a.taken_at, b.taken_at) {
        (Some(x), Some(y)) => (x - y).abs() <= 1,
        _ => true,
    };
    same_moment && ssim >= p.ssim_min_same_shape
}

/// Check every candidate against the pixels.
pub fn verify(
    files: &[FileInfo],
    cands: &[Candidate],
    store: &ThumbStore,
    p: &Params,
) -> VerifyReport {
    verify_controlled(files, cands, store, p, &pc_core::work::Control::default())
}
pub fn verify_controlled(
    files: &[FileInfo],
    cands: &[Candidate],
    store: &ThumbStore,
    p: &Params,
    control: &pc_core::work::Control,
) -> VerifyReport {
    let mut grays = Grays::new(store);
    let mut blank: HashMap<usize, bool> = HashMap::new();
    let mut report = VerifyReport::default();

    for c in cands {
        if control.current(&files[c.a].path).is_err() {
            break;
        }
        control.advance(0, None);
        // Screen out images with nothing to compare before trusting a score.
        let mut is_blank = false;
        for idx in [c.a, c.b] {
            let known = blank.get(&idx).copied();
            let flat = match known {
                Some(v) => v,
                None => {
                    let v = grays
                        .get(idx, files)
                        .map(|g| variance(g) < p.min_variance)
                        .unwrap_or(false);
                    blank.insert(idx, v);
                    v
                }
            };
            is_blank |= flat;
        }
        if is_blank {
            report.rejected_as_blank += 1;
            continue;
        }

        let Some(ga) = grays.get(c.a, files).cloned() else {
            report.thumbnails_missing += 1;
            continue;
        };
        let Some(gb) = grays.get(c.b, files) else {
            report.thumbnails_missing += 1;
            continue;
        };

        let s = pc_hash::ssim(&ga, gb);
        if s < p.ssim_min {
            report.rejected_by_ssim += 1;
            continue;
        }
        if !derivation_plausible(&files[c.a], &files[c.b], s, p) {
            report.rejected_as_series += 1;
            continue;
        }
        report.verified.push(Verified {
            a: c.a,
            b: c.b,
            ssim: s,
            phash_distance: c.phash_distance,
            via_crop: c.via_crop,
        });
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Luma;

    fn flat() -> GrayImage {
        GrayImage::from_pixel(64, 64, Luma([250]))
    }

    fn textured() -> GrayImage {
        GrayImage::from_fn(64, 64, |x, y| {
            Luma([(((x as f32 / 5.0).sin() + (y as f32 / 3.0).cos()) * 90.0 + 128.0) as u8])
        })
    }

    #[test]
    fn blankness_is_measured_not_guessed() {
        assert!(variance(&flat()) < 1.0);
        assert!(variance(&textured()) > 500.0);
    }

    fn f(w: i64, h: i64, container: &str, taken: Option<i64>) -> FileInfo {
        FileInfo {
            width: w,
            height: h,
            container: container.into(),
            taken_at: taken,
            ..Default::default()
        }
    }

    fn raw(taken: i64) -> FileInfo {
        FileInfo {
            width: 5456,
            height: 3632,
            container: "tiff".into(),
            pixel_source: "preview".into(),
            taken_at: Some(taken),
            ..Default::default()
        }
    }

    #[test]
    fn a_tripod_sequence_is_not_a_set_of_duplicates() {
        // Real numbers from nine frames of one scene shot minutes apart.
        let p = Params::default();
        let (a, b) = (raw(1_450_000_000), raw(1_450_000_300));
        for ssim in [0.860, 0.879, 0.921, 0.957] {
            assert!(
                !derivation_plausible(&a, &b, ssim, &p),
                "два кадра RAW слились при SSIM {ssim}"
            );
        }
    }

    #[test]
    fn a_rescaled_copy_is_a_plausible_derivative() {
        let p = Params::default();
        let big = f(6000, 4000, "jpeg", Some(1));
        let small = f(1200, 800, "jpeg", Some(1));
        assert!(derivation_plausible(&big, &small, 0.91, &p));
        assert!(!derivation_plausible(&big, &small, 0.80, &p));
    }

    #[test]
    fn a_format_conversion_is_a_plausible_derivative() {
        let p = Params::default();
        let a = f(4000, 3000, "jpeg", Some(1));
        let b = f(4000, 3000, "png", Some(1));
        assert!(derivation_plausible(&a, &b, 0.93, &p));
    }

    #[test]
    fn same_size_same_format_needs_a_near_perfect_match() {
        let p = Params::default();
        let a = f(4000, 3000, "jpeg", Some(1_000));
        let b = f(4000, 3000, "jpeg", Some(1_000));
        // A re-encode of the same pixels.
        assert!(derivation_plausible(&a, &b, 0.985, &p));
        // Two frames that merely look alike.
        assert!(!derivation_plausible(&a, &b, 0.93, &p));
    }

    #[test]
    fn two_frames_seconds_apart_are_not_merged_even_when_nearly_identical() {
        let p = Params::default();
        let a = f(4000, 3000, "jpeg", Some(1_000));
        let b = f(4000, 3000, "jpeg", Some(1_040));
        assert!(!derivation_plausible(&a, &b, 0.99, &p));
    }

    #[test]
    fn default_threshold_separates_blank_from_photographic() {
        let p = Params::default();
        assert!(variance(&flat()) < p.min_variance);
        assert!(variance(&textured()) > p.min_variance);
    }
}
