//! Scoring a file's claim to be the version worth keeping.
//!
//! The score is never the last word — it decides what the interface shows
//! first and which of several identical copies survives. It is deliberately
//! explainable: every term appears in the breakdown, because a number the
//! user cannot argue with is a number they cannot trust.

use pc_db::FileInfo;

/// Path fragments that mark a file as having been through something lossy.
const LOW_VALUE_PATH: [&str; 10] = [
    "telegram",
    "whatsapp",
    "viber",
    "downloads",
    "загрузки",
    "temp",
    "кэш",
    "cache",
    "thumb",
    "preview",
];

/// Name fragments a derivative tends to carry.
const LOW_VALUE_NAME: [&str; 8] = [
    "-edited",
    "_edited",
    "copy",
    "копия",
    "screenshot",
    "снимок экрана",
    "small",
    "web",
];

const PREFERRED_PATH: [&str; 5] = ["original", "оригинал", "raw", "исходник", "master"];

fn container_rank(f: &FileInfo) -> (f64, &'static str) {
    if f.is_raw() {
        return (22.0, "RAW");
    }
    match f.container.as_str() {
        "tiff" => (14.0, "TIFF"),
        "png" => (12.0, "PNG"),
        "psd" => (12.0, "PSD"),
        "heif" => (10.0, "HEIF"),
        "jpeg" => (8.0, "JPEG"),
        "webp" => (5.0, "WebP"),
        _ => (2.0, "прочее"),
    }
}

#[derive(Debug, Clone)]
pub struct Score {
    pub total: f64,
    pub breakdown: Vec<(String, f64)>,
}

impl Score {
    pub fn explain(&self) -> String {
        self.breakdown
            .iter()
            .map(|(k, v)| format!("{k} {v:+.0}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// `pixels` is the family's best, used to score resolution relatively.
pub fn score(f: &FileInfo, best_pixels: i64) -> Score {
    let mut parts: Vec<(String, f64)> = Vec::new();

    // Resolution, relative to the best in the family.
    let rel = if best_pixels > 0 {
        (f.pixels() as f64 / best_pixels as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    parts.push((format!("разрешение {}×{}", f.width, f.height), rel * 40.0));

    let (rank, label) = container_rank(f);
    parts.push((format!("формат {label}"), rank));

    if f.camera_model.is_some() {
        parts.push(("метаданные камеры".into(), 10.0));
    }
    if f.taken_at.is_some() {
        parts.push(("дата съёмки".into(), 4.0));
    }

    // Bytes per pixel, as a rough stand-in for encoding quality. Capped so a
    // bloated re-save cannot outrank a better original.
    if f.pixels() > 0 {
        let bpp = f.size as f64 / f.pixels() as f64;
        parts.push(("плотность данных".into(), (bpp * 4.0).clamp(0.0, 8.0)));
    }

    let path = f.path.to_ascii_lowercase();
    let name = f.name.to_ascii_lowercase();
    if let Some(hit) = LOW_VALUE_PATH.iter().find(|m| path.contains(**m)) {
        parts.push((format!("путь «{hit}»"), -15.0));
    }
    if let Some(hit) = LOW_VALUE_NAME.iter().find(|m| name.contains(**m)) {
        parts.push((format!("имя «{hit}»"), -10.0));
    }
    if let Some(hit) = PREFERRED_PATH.iter().find(|m| path.contains(**m)) {
        parts.push((format!("путь «{hit}»"), 8.0));
    }
    // "IMG_1234 (1).jpg" and "DSC01234 2.jpg" are what a copy looks like.
    if name.contains(" (") || name.contains(" 2.") || name.contains("(1)") {
        parts.push(("след копирования в имени".into(), -8.0));
    }

    let total = parts.iter().map(|(_, v)| v).sum();
    Score {
        total,
        breakdown: parts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, w: i64, h: i64, container: &str, size: i64) -> FileInfo {
        FileInfo {
            path: path.into(),
            name: path.rsplit_once('/').map_or(path, |(_, b)| b).into(),
            width: w,
            height: h,
            container: container.into(),
            size,
            ..Default::default()
        }
    }

    #[test]
    fn a_raw_original_outranks_its_export() {
        let mut raw = file("/foto/DSC01234.ARW", 6000, 4000, "tiff", 25_000_000);
        raw.pixel_source = "preview".into();
        raw.camera_model = Some("ILCE-7M3".into());
        let export = file("/foto/DSC01234-Edit.jpg", 6000, 4000, "jpeg", 4_000_000);
        let best = raw.pixels();
        assert!(score(&raw, best).total > score(&export, best).total);
    }

    #[test]
    fn a_messenger_copy_scores_far_below_the_original() {
        let orig = file("/foto/2019/DSC01234.JPG", 6000, 4000, "jpeg", 8_000_000);
        let tg = file("/Telegram/IMG_20190714.jpg", 1280, 853, "jpeg", 180_000);
        let best = orig.pixels();
        let (a, b) = (score(&orig, best).total, score(&tg, best).total);
        assert!(a > b + 30.0, "разрыв всего {:.0}", a - b);
    }

    #[test]
    fn the_breakdown_names_every_term_it_applied() {
        let tg = file("/Telegram/IMG (1).jpg", 800, 600, "jpeg", 90_000);
        let s = score(&tg, 24_000_000);
        let text = s.explain();
        assert!(text.contains("telegram"), "{text}");
        assert!(text.contains("след копирования"), "{text}");
    }

    #[test]
    fn density_cannot_let_a_bloated_resave_win() {
        let lean = file("/a/DSC1.jpg", 6000, 4000, "jpeg", 8_000_000);
        let bloated = file("/a/DSC2.jpg", 6000, 4000, "jpeg", 400_000_000);
        let best = lean.pixels();
        let diff = score(&bloated, best).total - score(&lean, best).total;
        assert!(diff <= 8.0, "плотность дала {diff:.0} преимущества");
    }
}
