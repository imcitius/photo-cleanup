//! Sorting the archive into kinds of picture.
//!
//! Everything here is measured, not recognised. Scans, documents and
//! screenshots have physical signatures — ink is bimodal, paper is
//! unsaturated, text produces contrast reversals by the hundred, a screen
//! grab has no camera behind it — and those can be read off the pixels for
//! nothing.
//!
//! What this cannot do is understand a picture. "A photograph of a utility
//! meter" is a semantic question, and answering it needs an embedding model.
//! The categories below stop exactly where measurement stops, and say so,
//! rather than guessing and being confidently wrong.

use anyhow::Result;
use pc_db::{Db, FileInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub enum Category {
    /// A scan, or a photograph of a printed page.
    Document,
    /// A capture of a screen rather than of the world.
    Screenshot,
    /// Nothing on it: a lens cap, a blown frame, an empty scan.
    Blank,
    /// A photograph with no colour in it. Informational, never rubbish.
    Monochrome,
    Photo,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Screenshot => "screenshot",
            Self::Blank => "blank",
            Self::Monochrome => "monochrome",
            Self::Photo => "photo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Document => pc_core::tr!("Документы и сканы", "Documents and scans"),
            Self::Screenshot => pc_core::tr!("Скриншоты", "Screenshots"),
            Self::Blank => pc_core::tr!("Пустые кадры", "Empty frames"),
            Self::Monochrome => pc_core::tr!("Чёрно-белое", "Monochrome"),
            Self::Photo => pc_core::tr!("Фотографии", "Photographs"),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "document" => Self::Document,
            "screenshot" => Self::Screenshot,
            "blank" => Self::Blank,
            "monochrome" => Self::Monochrome,
            "photo" => Self::Photo,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub category: Category,
    pub confidence: f32,
    pub evidence: Vec<String>,
}

/// Names a screenshot tends to carry, in the languages this archive uses.
///
/// Matched case-insensitively with `to_lowercase`, not its ASCII cousin:
/// that one leaves Cyrillic untouched, so "Снимок экрана" would never have
/// matched anything.
const SHOT_NAMES: [&str; 6] = [
    "screenshot",
    "screen shot",
    "снимок экрана",
    "скриншот",
    "снимок_экрана",
    "captura",
];

/// Exact pixel sizes of common displays and phone screens. A frame that
/// matches one of these and has no camera behind it is a screen grab.
const SCREEN_SIZES: [(i64, i64); 18] = [
    (1920, 1080),
    (2560, 1440),
    (3840, 2160),
    (1440, 900),
    (1680, 1050),
    (2880, 1800),
    (3024, 1964),
    (3456, 2234),
    (1280, 800),
    (2732, 2048),
    (2388, 1668),
    (2360, 1640),
    (1170, 2532),
    (1179, 2556),
    (1284, 2778),
    (1290, 2796),
    (1125, 2436),
    (750, 1334),
];

fn matches_screen(w: i64, h: i64) -> bool {
    SCREEN_SIZES
        .iter()
        .any(|&(a, b)| (w == a && h == b) || (w == b && h == a))
}

pub fn classify(f: &FileInfo) -> Verdict {
    let mut evidence = Vec::new();

    let saturation = f.saturation.unwrap_or(0.5) as f32;
    let white = f.white_fraction.unwrap_or(0.0) as f32;
    let bimodal = f.bimodality.unwrap_or(0.0) as f32;
    let text = f.text_rows.unwrap_or(0.0) as f32;
    let entropy = f.entropy.unwrap_or(8.0) as f32;
    let contrast = f.contrast.unwrap_or(50.0) as f32;

    // --- nothing on it ----------------------------------------------------
    if entropy < 2.0 && contrast < 12.0 {
        return Verdict {
            category: Category::Blank,
            confidence: 0.9,
            evidence: vec![pc_core::tf!(
                "энтропия {0:.1}, контраст {1:.0}",
                "entropy {0:.1}, contrast {1:.0}",
                entropy,
                contrast
            )],
        };
    }

    // --- a screen rather than the world -----------------------------------
    let name = f.name.to_lowercase();
    let named_shot = SHOT_NAMES.iter().any(|p| name.contains(p));
    let no_camera = f.camera_model.is_none();
    let screen_sized = matches_screen(f.width, f.height);

    if named_shot {
        evidence.push(
            pc_core::tr!(
                "имя файла говорит о снимке экрана",
                "the file name says screenshot"
            )
            .into(),
        );
    }
    if screen_sized && no_camera {
        evidence.push(pc_core::tf!(
            "размер экрана {0}×{1}, камеры нет",
            "screen-sized {0}×{1}, no camera",
            f.width,
            f.height
        ));
    }
    if named_shot || (screen_sized && no_camera) {
        return Verdict {
            category: Category::Screenshot,
            confidence: if named_shot && screen_sized {
                0.95
            } else {
                0.75
            },
            evidence,
        };
    }

    // --- ink on paper -----------------------------------------------------
    //
    // A scan is flat-lit: the paper is white, the histogram splits cleanly
    // into ink and page, and there is no colour in it. All four of those have
    // to hold at once, because each on its own describes half the archive.
    //
    // This deliberately does not try to find a page someone *photographed*.
    // An earlier version did, on the grounds that the line structure is
    // still visible in a dim room — and it called nine hundred photographs of
    // a sand arena documents. Measured against the real files, a photographed
    // page and a low-saturation landscape are the same numbers: entropy 7.3
    // against 7.5, bimodality 0.22 against 0.19, and the landscape scores
    // *more* line structure than the page, because grass and fencing produce
    // reversals by the hundred. There is no threshold in between. Telling a
    // notebook from a paddock is a question about meaning, which is the line
    // this module does not cross; see the note at the top.
    let banding = f.text_banding.unwrap_or(0.0) as f32;
    let printed = text > 0.008 && banding > 0.12;
    let scanned = printed && white > 0.40 && saturation < 0.14 && bimodal > 0.60;

    if scanned {
        let ratio = if f.height > 0 {
            f.width as f32 / f.height as f32
        } else {
            0.0
        };
        let a4 = (ratio - 1.414).abs() < 0.06 || (ratio - 0.707).abs() < 0.03;

        evidence.push(pc_core::tf!(
            "строчная структура {0:.3}",
            "line structure {0:.3}",
            text
        ));
        evidence.push(pc_core::tf!(
            "промежутки между строками {0:.0}%",
            "gaps between lines {0:.0}%",
            banding * 100.0
        ));
        evidence.push(pc_core::tf!(
            "бумага: белого {0:.0}%, ровный свет",
            "paper: {0:.0}% white, flat light",
            white * 100.0
        ));
        if a4 {
            evidence.push(pc_core::tr!("пропорции листа A4", "A4 page proportions").into());
        }
        let confidence = 0.70 + if a4 { 0.12 } else { 0.0 } + text.min(0.04) * 3.0;
        return Verdict {
            category: Category::Document,
            confidence: confidence.min(0.95),
            evidence,
        };
    }

    // --- colourless, but a photograph all the same ------------------------
    if saturation < 0.035 && entropy > 4.0 {
        return Verdict {
            category: Category::Monochrome,
            confidence: 0.7,
            evidence: vec![pc_core::tf!(
                "насыщенность {0:.3}",
                "saturation {0:.3}",
                saturation
            )],
        };
    }

    Verdict {
        category: Category::Photo,
        confidence: 0.5,
        evidence: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> FileInfo {
        FileInfo {
            id: 1,
            name: "DSC01234.JPG".into(),
            path: "/foto/DSC01234.JPG".into(),
            width: 6000,
            height: 4000,
            camera_model: Some("ILCE-7M3".into()),
            saturation: Some(0.28),
            white_fraction: Some(0.06),
            bimodality: Some(0.12),
            text_rows: Some(0.001),
            text_banding: Some(0.02),
            entropy: Some(7.4),
            contrast: Some(58.0),
            ..Default::default()
        }
    }

    fn document() -> FileInfo {
        FileInfo {
            name: "scan_0012.jpg".into(),
            width: 2480,
            height: 3508,
            camera_model: None,
            saturation: Some(0.02),
            white_fraction: Some(0.72),
            bimodality: Some(0.93),
            text_rows: Some(0.03),
            text_banding: Some(0.35),
            entropy: Some(3.1),
            contrast: Some(70.0),
            ..base()
        }
    }

    #[test]
    fn a_photograph_stays_a_photograph() {
        assert_eq!(classify(&base()).category, Category::Photo);
    }

    #[test]
    fn a_scanned_page_is_recognised_with_its_reasons() {
        let v = classify(&document());
        assert_eq!(v.category, Category::Document);
        assert!(v.confidence > 0.7, "{}", v.confidence);
        assert!(
            v.evidence.iter().any(|e| e.contains("A4")),
            "{:?}",
            v.evidence
        );
        assert!(v.evidence.iter().any(|e| e.contains("line structure")));
    }

    #[test]
    fn a_white_wall_is_not_a_document() {
        // Bright and unsaturated, but nothing is printed on it.
        let mut f = document();
        f.text_rows = Some(0.0005);
        assert_ne!(classify(&f).category, Category::Document);
    }

    #[test]
    fn a_picket_fence_is_not_a_document_either() {
        // Reversals on every row, but no gaps between lines: a fence, not
        // writing. This is the false positive that would otherwise sweep in
        // fences, brickwork and foliage by the thousand.
        let mut f = base();
        f.text_rows = Some(0.06);
        f.text_banding = Some(0.01);
        assert_ne!(classify(&f).category, Category::Document);
    }

    #[test]
    fn a_sand_arena_is_not_a_document() {
        // Real numbers from DSC05554.JPG, one of nine hundred photographs an
        // earlier version of this classifier filed under "documents and
        // scans". Dusty ground is unsaturated, fencing and grass produce
        // reversals by the hundred, and the sky above supplies the quiet rows
        // that pass for gaps between lines.
        let mut f = base();
        f.saturation = Some(0.202);
        f.white_fraction = Some(0.003);
        f.bimodality = Some(0.073);
        f.text_rows = Some(0.100);
        f.text_banding = Some(0.348);
        f.entropy = Some(7.29);
        assert_eq!(classify(&f).category, Category::Photo);
    }

    #[test]
    fn a_page_photographed_in_a_dim_room_is_left_as_a_photograph() {
        // Real numbers from a phone photograph of a notebook page. It is a
        // document, and it is not claimed as one: on these measurements it is
        // indistinguishable from the arena above, which scores *more* line
        // structure. Saying "photograph" and being wrong about a handful
        // beats saying "document" and being wrong about nine hundred.
        let mut f = base();
        f.name = "IMG_20180104_153945.jpg".into();
        f.saturation = Some(0.13);
        f.white_fraction = Some(0.00);
        f.bimodality = Some(0.04);
        f.text_rows = Some(0.0121);
        f.text_banding = Some(0.25);
        assert_eq!(classify(&f).category, Category::Photo);
    }

    #[test]
    fn a_colourful_photograph_with_texture_is_not_a_document() {
        let mut f = base();
        f.text_rows = Some(0.02);
        f.text_banding = Some(0.3);
        f.saturation = Some(0.45);
        assert_eq!(classify(&f).category, Category::Photo);
    }

    #[test]
    fn a_screenshot_is_caught_by_its_name() {
        let mut f = base();
        f.name = "Снимок экрана 2021-03-02 в 11.15.png".into();
        f.camera_model = None;
        assert_eq!(classify(&f).category, Category::Screenshot);
    }

    #[test]
    fn cyrillic_names_are_matched_case_insensitively() {
        // to_ascii_lowercase leaves Cyrillic alone, so a capitalised Russian
        // name silently matched nothing.
        for name in [
            "Снимок экрана 2021-03-02 в 11.15.png",
            "СНИМОК ЭКРАНА.png",
            "Скриншот игры.png",
        ] {
            let mut f = base();
            f.name = name.into();
            f.camera_model = None;
            assert_eq!(
                classify(&f).category,
                Category::Screenshot,
                "не распознано: {name}"
            );
        }
    }

    #[test]
    fn a_screenshot_is_caught_by_its_dimensions_without_a_name() {
        let mut f = base();
        f.name = "IMG_0042.png".into();
        f.camera_model = None;
        f.width = 1170;
        f.height = 2532;
        let v = classify(&f);
        assert_eq!(v.category, Category::Screenshot);
        assert!(v.evidence[0].contains("1170"));
    }

    #[test]
    fn a_photograph_that_happens_to_be_screen_sized_is_not_a_screenshot() {
        // The camera metadata is what settles it.
        let mut f = base();
        f.width = 1920;
        f.height = 1080;
        assert_eq!(classify(&f).category, Category::Photo);
    }

    #[test]
    fn an_empty_frame_is_flagged_rather_than_classified() {
        let mut f = base();
        f.entropy = Some(0.4);
        f.contrast = Some(2.0);
        assert_eq!(classify(&f).category, Category::Blank);
    }

    #[test]
    fn a_black_and_white_photograph_is_not_mistaken_for_a_scan() {
        let mut f = base();
        f.saturation = Some(0.01);
        f.white_fraction = Some(0.10);
        f.bimodality = Some(0.20);
        f.text_rows = Some(0.001);
        assert_eq!(classify(&f).category, Category::Monochrome);
    }

    #[test]
    fn a_file_with_no_measurements_is_left_as_a_photograph() {
        // Missing metrics must not be read as zeroes and land everything in
        // one bucket.
        let f = FileInfo {
            name: "unknown.jpg".into(),
            ..Default::default()
        };
        assert_eq!(classify(&f).category, Category::Photo);
    }
}

#[derive(Debug, Default)]
pub struct CategorizeReport {
    pub classified: usize,
    pub by_category: std::collections::BTreeMap<&'static str, usize>,
}

pub fn build(db: &Db) -> Result<CategorizeReport> {
    build_controlled(db, &pc_core::work::Control::default())
}
pub fn build_controlled(db: &Db, control: &pc_core::work::Control) -> Result<CategorizeReport> {
    let files = db.all_indexed()?;
    let mut report = CategorizeReport::default();
    control.begin(
        pc_core::tr!("Определение видов", "Working out the kinds"),
        files.len() as u64,
        0,
    )?;
    db.conn.execute_batch("BEGIN")?;
    for f in &files {
        control.check()?;
        control.advance(0, None);
        let v = classify(f);
        db.set_category(
            f.id,
            v.category.as_str(),
            v.confidence as f64,
            &v.evidence.join("; "),
        )?;
        report.classified += 1;
        *report.by_category.entry(v.category.label()).or_insert(0) += 1;
    }
    db.conn.execute_batch("COMMIT")?;
    Ok(report)
}
