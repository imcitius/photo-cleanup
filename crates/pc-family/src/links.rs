//! Exact links between files.
//!
//! Everything here is evidence a file itself carries, not a similarity score:
//! a DNG naming the raw it was converted from, Adobe's document chain, a
//! camera writing a JPEG beside a raw frame. Where these exist the grouping
//! is certain, and the perceptual stage only has to fill the gaps.

use pc_db::FileInfo;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum LinkKind {
    /// Identical bytes.
    SameBytes,
    /// Identical pixels, different container or metadata.
    SamePixels,
    /// DNG tag 0xC68B names the raw it came from.
    DngConversion,
    /// `xmpMM:DerivedFrom` points at the other file's document id.
    XmpDerived,
    /// Both descend from the same original document.
    XmpSibling,
    /// A camera wrote `DSC01234.ARW` and `DSC01234.JPG` together.
    RawJpegPair,
    /// Same body, same second, corroborated by appearance.
    SameShutter,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameBytes => "same-bytes",
            Self::SamePixels => "same-pixels",
            Self::DngConversion => "dng-conversion",
            Self::XmpDerived => "xmp-derived",
            Self::XmpSibling => "xmp-sibling",
            Self::RawJpegPair => "raw-jpeg-pair",
            Self::SameShutter => "same-shutter",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Link {
    pub a: usize,
    pub b: usize,
    pub kind: LinkKind,
    pub detail: String,
}

fn stem_lower(name: &str) -> String {
    name.rsplit_once('.')
        .map_or(name, |(a, _)| a)
        .to_lowercase()
}

/// Every exact link the files themselves assert.
pub fn detect(files: &[FileInfo]) -> Vec<Link> {
    let mut links = Vec::new();

    // --- identical bytes and identical pixels -----------------------------
    let mut by_partial: HashMap<&[u8], Vec<usize>> = HashMap::new();
    let mut by_pixels: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if let Some(h) = &f.partial_hash {
            by_partial.entry(h).or_default().push(i);
        }
        if let Some(h) = &f.pixel_hash {
            by_pixels.entry(h).or_default().push(i);
        }
    }
    for group in by_partial.values().filter(|g| g.len() > 1) {
        for w in group.windows(2) {
            links.push(Link {
                a: w[0],
                b: w[1],
                kind: LinkKind::SameBytes,
                detail: pc_core::tr!(
                    "совпали размер и оба конца файла",
                    "same size and the same bytes at both ends"
                )
                .into(),
            });
        }
    }
    for group in by_pixels.values().filter(|g| g.len() > 1) {
        for w in group.windows(2) {
            links.push(Link {
                a: w[0],
                b: w[1],
                kind: LinkKind::SamePixels,
                detail: pc_core::tr!(
                    "одинаковые пиксели после нормализации поворота",
                    "identical pixels once rotation is normalised"
                )
                .into(),
            });
        }
    }

    // --- DNG conversions --------------------------------------------------
    // Matched within a directory first, because Sony reuses file numbers:
    // DSC01234 exists many times over in an archive spanning years.
    let mut by_dir_stem: HashMap<(&str, String), Vec<usize>> = HashMap::new();
    let mut by_stem: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        by_dir_stem
            .entry((f.dir(), stem_lower(&f.name)))
            .or_default()
            .push(i);
        by_stem.entry(stem_lower(&f.name)).or_default().push(i);
    }
    for (i, f) in files.iter().enumerate() {
        let Some(orig) = &f.dng_original_raw else {
            continue;
        };
        let want = stem_lower(orig);
        let candidates = by_dir_stem
            .get(&(f.dir(), want.clone()))
            .or_else(|| by_stem.get(&want));
        for &j in candidates.into_iter().flatten() {
            if j != i {
                links.push(Link {
                    a: i,
                    b: j,
                    kind: LinkKind::DngConversion,
                    detail: format!("OriginalRawFileName = {orig}"),
                });
            }
        }
    }

    // --- Adobe's document chain ------------------------------------------
    let mut by_doc: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if let Some(d) = f.doc_id.as_deref() {
            by_doc.entry(d).or_default().push(i);
        }
    }
    for (i, f) in files.iter().enumerate() {
        if let Some(parent) = f.derived_from.as_deref() {
            for &j in by_doc.get(parent).into_iter().flatten() {
                if j != i {
                    links.push(Link {
                        a: i,
                        b: j,
                        kind: LinkKind::XmpDerived,
                        detail: format!("xmpMM:DerivedFrom = {parent}"),
                    });
                }
            }
        }
    }
    let mut by_orig: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if let Some(d) = f.orig_doc_id.as_deref() {
            by_orig.entry(d).or_default().push(i);
        }
    }
    for (id, group) in by_orig.iter().filter(|(_, g)| g.len() > 1) {
        for w in group.windows(2) {
            links.push(Link {
                a: w[0],
                b: w[1],
                kind: LinkKind::XmpSibling,
                detail: pc_core::tf!(
                    "общий OriginalDocumentID = {0}",
                    "shared OriginalDocumentID = {0}",
                    id
                ),
            });
        }
    }

    // --- raw + camera JPEG written together -------------------------------
    for group in by_dir_stem.values().filter(|g| g.len() > 1) {
        for (pos, &i) in group.iter().enumerate() {
            for &j in &group[pos + 1..] {
                let (a, b) = (&files[i], &files[j]);
                if a.container == b.container {
                    continue;
                }
                let same_moment = match (a.taken_at, b.taken_at) {
                    (Some(x), Some(y)) => (x - y).abs() <= 2,
                    // No date on either side: the shared name and directory
                    // still make a camera pair by far the likeliest reading.
                    (None, None) => true,
                    _ => false,
                };
                if same_moment && a.camera_model == b.camera_model {
                    links.push(Link {
                        a: i,
                        b: j,
                        kind: LinkKind::RawJpegPair,
                        detail: pc_core::tf!(
                            "одно имя {0} и один момент съёмки",
                            "same name {0} and the same moment",
                            a.stem()
                        ),
                    });
                }
            }
        }
    }

    links
}

/// Same body and same second, used only where appearance agrees.
///
/// On its own this rule would merge a burst: ten frames a second all carry
/// the same `DateTimeOriginal`. The perceptual check is what keeps distinct
/// frames apart.
pub fn shutter_candidates(files: &[FileInfo]) -> Vec<(usize, usize)> {
    let mut by_moment: HashMap<(&str, i64), Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        if let (Some(serial), Some(t)) = (f.body_serial.as_deref(), f.taken_at) {
            by_moment.entry((serial, t)).or_default().push(i);
        }
    }
    let mut out = Vec::new();
    for group in by_moment.values().filter(|g| g.len() > 1) {
        for (pos, &i) in group.iter().enumerate() {
            for &j in &group[pos + 1..] {
                out.push((i, j));
            }
        }
    }
    out
}
