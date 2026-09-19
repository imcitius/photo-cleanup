//! Assigning a role to every file in a family.
//!
//! The point of the roles is that "duplicate" is the wrong word for most of
//! what a photographer's archive contains. A raw frame, the JPEG the camera
//! wrote beside it, a DNG conversion and a Lightroom export are four
//! different objects describing one photograph, and only an exact copy of
//! one of them is rubbish.

use pc_db::FileInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub enum Role {
    Original,
    CameraJpeg,
    Converted,
    Export,
    Resize,
    Copy,
    Unknown,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::CameraJpeg => "camera-jpg",
            Self::Converted => "converted",
            Self::Export => "export",
            Self::Resize => "resize",
            Self::Copy => "copy",
            Self::Unknown => "unknown",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Original => "ORIGINAL",
            Self::CameraJpeg => "CAMERA JPG",
            Self::Converted => "CONVERTED",
            Self::Export => "EXPORT",
            Self::Resize => "RESIZE",
            Self::Copy => "COPY",
            Self::Unknown => "?",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "original" => Self::Original,
            "camera-jpg" => Self::CameraJpeg,
            "converted" => Self::Converted,
            "export" => Self::Export,
            "resize" => Self::Resize,
            "copy" => Self::Copy,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// Only an exact copy is rubbish by default. Everything else is a
    /// distinct rendition of the photograph and is kept unless the user
    /// decides otherwise.
    pub fn removable_by_default(self) -> bool {
        self == Self::Copy
    }
}

/// How much a role weighs when choosing which member to show first.
///
/// Kept apart from the quality score, which is computed before roles exist
/// and is used to decide which of several identical copies survives.
pub fn keeper_bonus(role: Role) -> f64 {
    match role {
        Role::Original => 30.0,
        Role::CameraJpeg => 15.0,
        Role::Converted => 12.0,
        Role::Export => 8.0,
        Role::Unknown => 0.0,
        Role::Resize => -10.0,
        Role::Copy => -40.0,
    }
}

const EDITORS: [&str; 6] = [
    "lightroom",
    "photoshop",
    "capture one",
    "camera raw",
    "gimp",
    "affinity",
];

fn looks_edited(f: &FileInfo) -> bool {
    f.derived_from.is_some()
        || f.software
            .as_deref()
            .map(|s| s.to_lowercase())
            .is_some_and(|s| EDITORS.iter().any(|e| s.contains(e)))
}

fn has_camera_exif(f: &FileInfo) -> bool {
    f.camera_model.is_some()
}

/// Assign roles to one family. `members` indexes into `files`.
///
/// `quality` decides which of several byte-identical members survives as the
/// real thing and which become copies.
pub fn assign(files: &[FileInfo], members: &[usize], quality: &[f64]) -> Vec<Role> {
    let mut roles = vec![Role::Unknown; members.len()];
    if members.is_empty() {
        return roles;
    }

    // Exact copies first: among members sharing pixels, the best-scoring one
    // stays and the rest are copies of it.
    let mut claimed = vec![false; members.len()];
    for pos in 0..members.len() {
        if claimed[pos] {
            continue;
        }
        let Some(hash) = files[members[pos]].pixel_hash.as_ref() else {
            continue;
        };
        let twins: Vec<usize> = (0..members.len())
            .filter(|&q| files[members[q]].pixel_hash.as_ref() == Some(hash))
            .collect();
        if twins.len() < 2 {
            continue;
        }
        // Identical twins score identically, so the tie-break decides which
        // path is treated as the real one. It must be deterministic, and it
        // must prefer the copy that is not buried in an archive folder.
        let best = *twins
            .iter()
            .max_by(|&&x, &&y| {
                let key = |i: usize| {
                    let p = &files[members[i]].path;
                    (
                        quality[i],
                        -(p.matches('/').count() as f64),
                        -(p.len() as f64),
                    )
                };
                key(x)
                    .partial_cmp(&key(y))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| files[members[y]].path.cmp(&files[members[x]].path))
            })
            .unwrap();
        for t in twins {
            claimed[t] = true;
            if t != best {
                roles[t] = Role::Copy;
            }
        }
    }

    // The original, if the family even contains one. A folder holding two
    // Lightroom exports of the same frame has no original in it, and calling
    // the larger export "the original" would be a plain untruth — the kind
    // the interface would then repeat to the user.
    let original = (0..members.len())
        .filter(|&i| roles[i] != Role::Copy)
        .filter(|&i| {
            let f = &files[members[i]];
            // A file that names the raw it was converted from has told us it
            // is not the original, whatever else it looks like.
            f.dng_original_raw.is_none() && (f.is_raw() || (has_camera_exif(f) && !looks_edited(f)))
        })
        .max_by_key(|&i| {
            let f = &files[members[i]];
            (f.is_raw(), f.pixels())
        });
    if let Some(i) = original {
        roles[i] = Role::Original;
    }

    let best_pixels = members
        .iter()
        .map(|&m| files[m].pixels())
        .max()
        .unwrap_or(0)
        .max(1);
    let original_stem = original.map(|i| files[members[i]].stem().to_lowercase());

    for i in 0..members.len() {
        if roles[i] != Role::Unknown {
            continue;
        }
        let f = &files[members[i]];

        // A conversion is a file that says what it was converted from.
        // A second raw frame is a second shutter press, not a rendition of
        // the first, so raw-ness alone must never imply this role.
        if f.dng_original_raw.is_some() {
            roles[i] = Role::Converted;
            continue;
        }

        if looks_edited(f) {
            roles[i] = Role::Export;
            continue;
        }

        // A camera JPEG: same name as the original, untouched camera
        // metadata, and full size.
        let same_name = original_stem
            .as_deref()
            .is_some_and(|s| s == f.stem().to_lowercase());
        if same_name && has_camera_exif(f) && f.pixels() * 10 >= best_pixels * 6 {
            roles[i] = Role::CameraJpeg;
            continue;
        }

        // Anything meaningfully smaller than the best is a derivative.
        if f.pixels() * 10 < best_pixels * 6 {
            roles[i] = Role::Resize;
            continue;
        }

        roles[i] = if f.is_raw() {
            // Another raw beside the original: a distinct frame that the
            // grouping pulled in, not a rendition of anything.
            Role::Original
        } else if has_camera_exif(f) {
            Role::CameraJpeg
        } else {
            Role::Unknown
        };
    }

    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(name: &str, w: i64, h: i64, size: i64) -> FileInfo {
        FileInfo {
            path: format!("/foto/{name}"),
            name: name.into(),
            container: "jpeg".into(),
            width: w,
            height: h,
            size,
            camera_model: Some("ILCE-6700".into()),
            ..Default::default()
        }
    }

    fn raw_file(name: &str) -> FileInfo {
        FileInfo {
            path: format!("/foto/{name}"),
            name: name.into(),
            container: "tiff".into(),
            pixel_source: "preview".into(),
            width: 6000,
            height: 4000,
            size: 25_000_000,
            camera_model: Some("ILCE-7M3".into()),
            ..Default::default()
        }
    }

    fn roles_of(files: &[FileInfo]) -> Vec<Role> {
        let members: Vec<usize> = (0..files.len()).collect();
        let q: Vec<f64> = files.iter().map(|f| f.pixels() as f64).collect();
        assign(files, &members, &q)
    }

    #[test]
    fn a_raw_and_the_camera_jpeg_beside_it_are_not_duplicates() {
        let files = vec![
            raw_file("DSC01234.ARW"),
            jpeg("DSC01234.JPG", 6000, 4000, 8_000_000),
        ];
        let r = roles_of(&files);
        assert_eq!(r[0], Role::Original);
        assert_eq!(r[1], Role::CameraJpeg);
        assert!(r.iter().all(|x| !x.removable_by_default()));
    }

    #[test]
    fn two_exports_of_one_frame_have_no_original_between_them() {
        // Both written by Lightroom into different folders: neither is the
        // photograph, so neither may be labelled as such.
        let mut a = jpeg("DSC01147.jpg", 4000, 2667, 1_200_000);
        let mut b = jpeg("DSC01147.jpg", 4000, 2667, 5_400_000);
        a.software = Some("Adobe Photoshop Lightroom Classic 14.1.1".into());
        b.software = Some("Adobe Photoshop Lightroom Classic 14.1.1".into());
        let r = roles_of(&[a, b]);
        assert_eq!(r, vec![Role::Export, Role::Export]);
        assert!(!r.contains(&Role::Original));
    }

    #[test]
    fn an_exact_copy_is_the_only_role_removed_by_default() {
        let mut a = jpeg("DSC1.jpg", 4000, 3000, 5_000_000);
        let mut b = jpeg("DSC1 (1).jpg", 4000, 3000, 5_000_000);
        a.pixel_hash = Some(vec![7; 32]);
        b.pixel_hash = Some(vec![7; 32]);
        let members = vec![0usize, 1];
        // The first scores higher, so the second becomes the copy.
        let r = assign(&[a, b], &members, &[100.0, 50.0]);
        assert_eq!(r[0], Role::Original);
        assert_eq!(r[1], Role::Copy);
        assert!(r[1].removable_by_default());
        assert!(!r[0].removable_by_default());
    }

    #[test]
    fn a_much_smaller_rendition_is_a_resize() {
        let files = vec![
            raw_file("DSC01234.ARW"),
            jpeg("DSC01234_web.jpg", 1200, 800, 200_000),
        ];
        assert_eq!(roles_of(&files)[1], Role::Resize);
    }

    #[test]
    fn a_dng_that_names_its_source_is_a_conversion() {
        let mut dng = raw_file("DSC01234.dng");
        dng.dng_original_raw = Some("DSC01234.ARW".into());
        let r = roles_of(&[raw_file("DSC01234.ARW"), dng]);
        assert_eq!(r[1], Role::Converted);
    }

    #[test]
    fn the_keeper_bonus_prefers_an_original_over_an_export() {
        assert!(keeper_bonus(Role::Original) > keeper_bonus(Role::Export));
        assert!(keeper_bonus(Role::Copy) < keeper_bonus(Role::Resize));
    }
}
