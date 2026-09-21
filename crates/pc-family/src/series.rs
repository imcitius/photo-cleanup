//! Series: several shutter presses of one moment, and which frame to keep.
//!
//! This is the opposite job to deduplication, and the two must not be
//! confused. A burst is not waste — every frame is a distinct photograph,
//! and the tool's part is to say which one came out best and let the user
//! decide what to do about the rest. Nothing here is ever proposed for
//! removal on its own.

use anyhow::Result;
use pc_db::{Db, FileInfo};

/// Frames further apart than this belong to different moments.
const DEFAULT_GAP_SECS: i64 = 10;
/// A sequence needs at least this many frames to be worth calling a series.
const MIN_MEMBERS: usize = 2;
/// Frames must still look like each other; a gap in time is not enough.
const MAX_PHASH_DISTANCE: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeriesKind {
    /// Frames seconds apart: a burst, or a few tries at one shot.
    Burst,
    /// Same moment, different exposure: bracketing for HDR.
    Bracket,
    /// Sony's sensor-shift capture. Four frames that merge into one image,
    /// visually identical and ruinous to thin.
    PixelShift,
}

impl SeriesKind {
    /// The word stored in the database, back into a kind.
    pub fn parse(s: &str) -> Self {
        match s {
            "pixel-shift" => Self::PixelShift,
            "bracket" => Self::Bracket,
            _ => Self::Burst,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Burst => "burst",
            Self::Bracket => "bracket",
            Self::PixelShift => "pixel-shift",
        }
    }

    /// What to call this on screen.
    ///
    /// Two of these are guesses and are worded as guesses. Nothing here reads
    /// the camera's own account of what it was doing: a pixel-shift set is
    /// recognised by four indistinguishable raw frames in a moment, and
    /// bracketing by frames of one moment clipping differently. Ordinary
    /// shooting produces both of those often enough — four quick frames of a
    /// still subject, a moving scene against a bright sky — and a label that
    /// says "pixel-shift" flatly claims a fact the tool does not have.
    pub fn label(self) -> &'static str {
        match self {
            Self::Burst => pc_core::tr!("серия", "burst"),
            Self::Bracket => pc_core::tr!("похоже на брекетинг", "looks like bracketing"),
            Self::PixelShift => pc_core::tr!("похоже на pixel-shift", "looks like pixel-shift"),
        }
    }

    /// Why the tool thinks so, in the words of what was actually measured.
    pub fn because(self) -> &'static str {
        match self {
            Self::Burst => pc_core::tr!(
                "кадры сняты один за другим",
                "the frames were taken one after another"
            ),
            Self::Bracket => pc_core::tr!(
                "кадры одного мгновения по-разному пересвечены — так снимают с вилкой экспозиции",
                "frames of one moment clip differently, which is how an exposure bracket looks"
            ),
            Self::PixelShift => pc_core::tr!(
                "четыре неразличимых RAW за пару секунд — так выглядит съёмка со сдвигом матрицы",
                "four indistinguishable raw frames within a couple of seconds, which is how a sensor-shift capture looks"
            ),
        }
    }

    /// Whether thinning the series is forbidden outright.
    pub fn protected(self) -> bool {
        // A pixel-shift set is one photograph stored as four files; losing
        // any of them loses the photograph. The guess is not certain, so the
        // protection errs the safe way: a burst kept whole costs the user a
        // decision, a pixel-shift set thinned costs them the photograph.
        matches!(self, Self::PixelShift)
    }
}

#[derive(Debug, Clone)]
pub struct Ranked {
    pub file_id: i64,
    pub score: f64,
    pub breakdown: String,
}

#[derive(Debug, Clone)]
pub struct Series {
    pub kind: SeriesKind,
    pub started_at: Option<i64>,
    pub camera: Option<String>,
    pub members: Vec<Ranked>,
}

impl Series {
    pub fn best(&self) -> Option<&Ranked> {
        self.members.first()
    }
}

/// Score one frame against the others in its series.
///
/// Only relative values matter, so every term is normalised against the best
/// in the group: sharpness in absolute units means nothing across cameras or
/// resolutions, but "sharpest of these five" means everything.
fn rank(members: &[&FileInfo]) -> Vec<Ranked> {
    let best_sharp = members
        .iter()
        .filter_map(|f| f.sharpness)
        .fold(0.0f64, f64::max)
        .max(1e-6);
    let best_entropy = members
        .iter()
        .filter_map(|f| f.entropy)
        .fold(0.0f64, f64::max)
        .max(1e-6);

    let mut out: Vec<Ranked> = members
        .iter()
        .map(|f| {
            let mut parts: Vec<(String, f64)> = Vec::new();

            let sharp = f.sharpness.unwrap_or(0.0) / best_sharp;
            parts.push((pc_core::tr!("резкость", "sharpness").into(), sharp * 60.0));

            // Clipping is punished rather than rewarded: a frame with blown
            // highlights has thrown information away for good.
            let clipped = f.clip_high.unwrap_or(0.0) + f.clip_low.unwrap_or(0.0) * 0.5;
            if clipped > 0.005 {
                parts.push((
                    pc_core::tf!(
                        "потери в светах и тенях {0:.1}%",
                        "clipped highlights and shadows {0:.1}%",
                        clipped * 100.0
                    ),
                    -(clipped.min(0.25) * 80.0),
                ));
            }

            let ent = f.entropy.unwrap_or(0.0) / best_entropy;
            parts.push((pc_core::tr!("детализация", "detail").into(), ent * 20.0));

            let total: f64 = parts.iter().map(|(_, v)| v).sum();
            Ranked {
                file_id: f.id,
                score: total,
                breakdown: parts
                    .iter()
                    .map(|(k, v)| format!("{k} {v:+.0}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        })
        .collect();

    // Deterministic: equal scores fall back to capture order, then id.
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.file_id.cmp(&b.file_id))
    });
    out
}

/// What kind of series this looks like.
///
/// Every answer here is an inference from timing and from measurements of the
/// frames. The camera's own record of its mode is not read — the makers keep
/// it in their own notes, which this tool does not parse — so these are named
/// as resemblances, not as facts, wherever they are shown.
fn classify(members: &[&FileInfo]) -> SeriesKind {
    // Four indistinguishable raw frames in a moment: what sensor shift looks
    // like, and also what four quick frames of a still subject look like.
    let span = match (
        members.iter().filter_map(|f| f.taken_at).min(),
        members.iter().filter_map(|f| f.taken_at).max(),
    ) {
        (Some(a), Some(b)) => b - a,
        _ => i64::MAX,
    };
    let all_raw = members.iter().all(|f| f.is_raw());
    if all_raw && members.len() == 4 && span <= 2 {
        return SeriesKind::PixelShift;
    }
    // Bracketing shifts exposure deliberately, so the frames differ in how
    // much they clip while looking otherwise alike.
    let clip: Vec<f64> = members
        .iter()
        .map(|f| f.clip_high.unwrap_or(0.0) + f.clip_low.unwrap_or(0.0))
        .collect();
    let spread =
        clip.iter().cloned().fold(0.0f64, f64::max) - clip.iter().cloned().fold(f64::MAX, f64::min);
    if span <= 3 && spread > 0.08 {
        return SeriesKind::Bracket;
    }
    SeriesKind::Burst
}

/// Group frames into series and rank each one.
pub fn detect(files: &[FileInfo], gap_secs: i64) -> Vec<Series> {
    // Only real frames take part: an export or a resized copy is not another
    // press of the shutter.
    let mut frames: Vec<&FileInfo> = files
        .iter()
        .filter(|f| f.taken_at.is_some() && f.camera_model.is_some())
        .collect();
    frames.sort_by_key(|f| (f.camera_model.clone(), f.taken_at, f.id));

    let mut out = Vec::new();
    let mut run: Vec<&FileInfo> = Vec::new();

    let flush = |run: &mut Vec<&FileInfo>, out: &mut Vec<Series>| {
        if run.len() >= MIN_MEMBERS {
            let kind = classify(run);
            out.push(Series {
                kind,
                started_at: run.iter().filter_map(|f| f.taken_at).min(),
                camera: run[0].camera_model.clone(),
                members: rank(run),
            });
        }
        run.clear();
    };

    for f in frames {
        let continues = run.last().is_some_and(|prev| {
            prev.camera_model == f.camera_model
                && matches!((prev.taken_at, f.taken_at), (Some(a), Some(b)) if (b - a).abs() <= gap_secs)
                // Consecutive in time is not enough: two unrelated shots a
                // few seconds apart are two photographs, not a series.
                && pc_hash::hamming(prev.phash, f.phash) <= MAX_PHASH_DISTANCE
        });
        if !continues {
            flush(&mut run, &mut out);
        }
        run.push(f);
    }
    flush(&mut run, &mut out);

    out.sort_by_key(|s| s.started_at);
    out
}

#[derive(Debug, Default)]
pub struct SeriesReport {
    pub series: usize,
    pub frames: usize,
    pub protected: usize,
    /// Groups of copies whose keeper moved to the folder holding the rest of
    /// its burst.
    pub keepers_settled: u64,
    pub by_kind: std::collections::BTreeMap<&'static str, usize>,
}

pub fn build(db: &Db, gap_secs: i64) -> Result<SeriesReport> {
    build_controlled(db, gap_secs, &pc_core::work::Control::default())
}
pub fn build_controlled(
    db: &Db,
    gap_secs: i64,
    control: &pc_core::work::Control,
) -> Result<SeriesReport> {
    let files = db.all_indexed()?;
    let manual: std::collections::HashSet<i64> = db
        .conn
        .prepare("SELECT file_id FROM manual_best")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let found = detect(
        &files,
        if gap_secs > 0 {
            gap_secs
        } else {
            DEFAULT_GAP_SECS
        },
    );

    let run_id = db.latest_run()?.unwrap_or(0);
    let mut report = SeriesReport::default();

    control.begin(
        pc_core::tr!("Сохранение серий", "Saving the bursts"),
        found.len() as u64,
        0,
    )?;
    db.conn.execute_batch("BEGIN")?;
    db.clear_series()?;
    for s in &found {
        control.check()?;
        control.advance(0, None);
        let best = s
            .members
            .iter()
            .find(|m| manual.contains(&m.file_id))
            .map(|m| m.file_id)
            .or_else(|| s.best().map(|r| r.file_id));
        let id = db.insert_series(
            s.kind.as_str(),
            s.started_at,
            s.camera.as_deref(),
            best,
            s.kind.protected(),
            run_id,
        )?;
        for (i, m) in s.members.iter().enumerate() {
            db.insert_series_member(id, m.file_id, i as i64, m.score, &m.breakdown)?;
        }
        report.series += 1;
        report.frames += s.members.len();
        if s.kind.protected() {
            report.protected += 1;
        }
        *report.by_kind.entry(s.kind.label()).or_insert(0) += 1;
    }
    // Now that the bursts are known, the groups of copies can be settled in
    // their favour: between identical files, keep the one where the rest of
    // the burst lives.
    report.keepers_settled = crate::folders::settle_keepers(db)?;
    db.conn.execute_batch("COMMIT")?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_guess_about_the_camera_is_worded_as_a_guess() {
        // Four quick raw frames of a still subject look exactly like a
        // sensor-shift capture, and nothing here reads the camera's own
        // account of what it was doing. The set is still protected — that
        // errs the safe way — but the screen must not state as fact a mode
        // that was never measured.
        for kind in [SeriesKind::PixelShift, SeriesKind::Bracket] {
            let label = kind.label();
            assert!(
                label.starts_with("похоже") || label.starts_with("looks like"),
                "метка утверждает режим камеры: {label}"
            );
            assert!(!kind.because().is_empty(), "не сказано, что измерено");
        }
        assert_eq!(SeriesKind::Burst.label(), pc_core::tr!("серия", "burst"));
        assert!(SeriesKind::PixelShift.protected());
        assert!(!SeriesKind::Bracket.protected());
    }

    #[test]
    fn the_word_in_the_database_survives_the_round_trip() {
        for kind in [
            SeriesKind::Burst,
            SeriesKind::Bracket,
            SeriesKind::PixelShift,
        ] {
            assert_eq!(SeriesKind::parse(kind.as_str()), kind);
        }
    }

    use super::*;

    fn frame(id: i64, t: i64, sharp: f64, phash: u64) -> FileInfo {
        FileInfo {
            id,
            path: format!("/foto/DSC0{id}.JPG"),
            name: format!("DSC0{id}.JPG"),
            camera_model: Some("ILCE-7M3".into()),
            taken_at: Some(t),
            phash,
            sharpness: Some(sharp),
            entropy: Some(7.0),
            clip_high: Some(0.0),
            clip_low: Some(0.0),
            width: 6000,
            height: 4000,
            ..Default::default()
        }
    }

    #[test]
    fn frames_seconds_apart_form_one_series() {
        let files = vec![
            frame(1, 1000, 10.0, 0b1010),
            frame(2, 1002, 30.0, 0b1011),
            frame(3, 1004, 20.0, 0b1010),
        ];
        let s = detect(&files, 10);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].members.len(), 3);
    }

    #[test]
    fn the_sharpest_frame_wins() {
        let files = vec![
            frame(1, 1000, 10.0, 0b1010),
            frame(2, 1002, 30.0, 0b1011),
            frame(3, 1004, 20.0, 0b1010),
        ];
        let s = detect(&files, 10);
        assert_eq!(s[0].best().unwrap().file_id, 2);
        assert!(s[0].best().unwrap().breakdown.contains("sharpness"));
    }

    #[test]
    fn a_long_gap_starts_a_new_series() {
        let files = vec![
            frame(1, 1000, 10.0, 0b1010),
            frame(2, 1002, 10.0, 0b1010),
            frame(3, 5000, 10.0, 0b1010),
            frame(4, 5002, 10.0, 0b1010),
        ];
        assert_eq!(detect(&files, 10).len(), 2);
    }

    #[test]
    fn two_unrelated_shots_moments_apart_are_not_a_series() {
        // Consecutive in time, nothing alike: a photograph of one thing and
        // then of another.
        let files = vec![
            frame(1, 1000, 10.0, 0x0000_0000_0000_0000),
            frame(2, 1003, 10.0, 0xFFFF_FFFF_FFFF_FFFF),
        ];
        assert!(detect(&files, 10).is_empty());
    }

    #[test]
    fn a_blown_frame_is_punished_even_when_sharp() {
        let mut files = vec![frame(1, 1000, 30.0, 0b1010), frame(2, 1002, 28.0, 0b1010)];
        files[0].clip_high = Some(0.20);
        let s = detect(&files, 10);
        assert_eq!(s[0].best().unwrap().file_id, 2, "выбран пересвеченный кадр");
        assert!(s[0].members[1].breakdown.contains("clipped"));
    }

    #[test]
    fn a_pixel_shift_set_is_recognised_and_protected() {
        // Four raw frames inside a second, visually identical.
        let files: Vec<FileInfo> = (1..=4)
            .map(|i| {
                let mut f = frame(i, 1000 + i / 4, 20.0, 0b1010);
                f.container = "tiff".into();
                f.pixel_source = "preview".into();
                f
            })
            .collect();
        let s = detect(&files, 10);
        assert_eq!(s[0].kind, SeriesKind::PixelShift);
        assert!(s[0].kind.protected());
    }

    #[test]
    fn bracketing_is_told_apart_by_its_exposure_spread() {
        let mut files = vec![
            frame(1, 1000, 20.0, 0b1010),
            frame(2, 1001, 20.0, 0b1010),
            frame(3, 1002, 20.0, 0b1010),
        ];
        files[0].clip_low = Some(0.18);
        files[2].clip_high = Some(0.15);
        assert_eq!(detect(&files, 10)[0].kind, SeriesKind::Bracket);
    }

    #[test]
    fn frames_from_different_cameras_never_join() {
        let mut files = vec![frame(1, 1000, 10.0, 0b1010), frame(2, 1001, 10.0, 0b1010)];
        files[1].camera_model = Some("X100V".into());
        assert!(detect(&files, 10).is_empty());
    }

    #[test]
    fn a_single_frame_is_not_a_series() {
        assert!(detect(&[frame(1, 1000, 10.0, 0b1010)], 10).is_empty());
    }

    #[test]
    fn ranking_is_deterministic_when_scores_tie() {
        let files = vec![frame(2, 1000, 10.0, 0b1010), frame(1, 1001, 10.0, 0b1010)];
        let a = detect(&files, 10)[0].members[0].file_id;
        let b = detect(&files, 10)[0].members[0].file_id;
        assert_eq!(a, b);
        assert_eq!(a, 1, "при равенстве ожидается меньший id");
    }
}
