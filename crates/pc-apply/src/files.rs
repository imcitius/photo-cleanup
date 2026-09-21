//! Quarantining photographs, as opposed to regenerable caches.
//!
//! The bar here is much higher than for a Lightroom preview. A preview costs
//! time to rebuild; a photograph cannot be rebuilt at all. So nothing moves
//! until the tool has re-read the pixels and confirmed, at that moment, that
//! the frame survives somewhere else.

use anyhow::{bail, Context, Result};
use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{rename_with_parents, Totals};

/// Files that belong to a photograph and must travel with it.
///
/// An orphaned `.xmp` left behind is a Lightroom edit pointing at nothing,
/// and an orphaned AppleDouble is litter.
pub fn companions(path: &Path) -> Vec<PathBuf> {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let stem = name.rsplit_once('.').map_or(name, |(a, _)| a);

    let mut out = Vec::new();
    let mut identities = std::collections::HashSet::new();
    for candidate in [
        format!("{stem}.xmp"),
        format!("{stem}.XMP"),
        format!("{name}.xmp"),
        format!("{stem}.aae"),
        format!("{stem}.AAE"),
        format!("{name}.pp3"),
        format!("._{name}"),
    ] {
        let p = dir.join(candidate);
        if let Ok(md) = fs::metadata(&p) {
            // On case-insensitive filesystems .xmp and .XMP can address
            // one directory entry. Count and move that companion once.
            if md.is_file() && identities.insert(pc_core::volume::entry_key(&md, &p)) {
                out.push(p);
            }
        }
    }
    out
}

/// Confirm that two files really do hold the same picture.
///
/// This re-reads and re-derives the pixel hash rather than trusting what the
/// index recorded. The index may be hours old; this is the last moment before
/// something becomes hard to undo, and it is the moment worth paying for.
pub fn same_picture(a: &Path, b: &Path) -> Result<bool> {
    let read = |p: &Path| -> Result<(pc_image::Probe, bool)> {
        let size = fs::metadata(p)
            .with_context(|| pc_core::tf!("нет файла {0}", "no such file: {0}", p.display()))?
            .len();
        let r = pc_image::read_for_probe(p, size)?;
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let probe = pc_image::probe_parts(p, &r.head, r.preview.as_deref(), name)?;
        let from_preview = matches!(probe.source, pc_image::PixelSource::EmbeddedPreview { .. });
        Ok((probe, from_preview))
    };
    let (pa, a_preview) = read(a)?;
    let (pb, b_preview) = read(b)?;

    // A raw file is never opened whole: what is decoded is the JPEG the
    // camera left inside it, and two different frames can carry previews that
    // agree. Where the pixels are only a preview, the files themselves have
    // to match — read in full, which costs a read of two files at the one
    // moment where the cost is obviously worth paying.
    if a_preview || b_preview {
        return same_bytes(a, b);
    }
    Ok(pa.content_hash == pb.content_hash)
}

/// Whole files, compared as they are. Sizes first, because a difference there
/// is free to find and ends the question.
fn same_bytes(a: &Path, b: &Path) -> Result<bool> {
    use std::io::Read;
    let (ma, mb) = (fs::metadata(a)?, fs::metadata(b)?);
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (fs::File::open(a)?, fs::File::open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let read_a = fa.read(&mut ba)?;
        let read_b = fb.read(&mut bb)?;
        if read_a != read_b {
            return Ok(false);
        }
        if read_a == 0 {
            return Ok(true);
        }
        if ba[..read_a] != bb[..read_b] {
            return Ok(false);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    Moved,
    /// Verification did not hold; nothing was touched.
    Refused,
}

pub fn quarantine_file(
    db: &Db,
    run_id: i64,
    c: &Candidate,
    override_root: Option<&Path>,
) -> Result<(FileOutcome, String)> {
    let src = Path::new(&c.path);
    let keeper = Path::new(&c.keeper_path);

    if !src.is_file() {
        return Ok((
            FileOutcome::Refused,
            pc_core::tr!("файла уже нет", "the file is already gone").into(),
        ));
    }

    // A candidate the tool picked has to prove itself: the file that makes it
    // redundant must still exist and still hold the same pixels. A candidate
    // the *user* picked has no such twin and needs none — the justification is
    // that they looked at the frame and did not want it.
    if !c.manual {
        if !keeper.is_file() {
            return Ok((
                FileOutcome::Refused,
                pc_core::tf!(
                    "нет файла, ради которого удаляем: {0}",
                    "the file this one is redundant to is missing: {0}",
                    c.keeper_path
                ),
            ));
        }
        if src == keeper {
            return Ok((
                FileOutcome::Refused,
                pc_core::tr!("это и есть сохраняемый файл", "this is the file being kept").into(),
            ));
        }
        // Re-read both and compare the pixels as they are right now.
        match same_picture(src, keeper) {
            Ok(true) => {}
            Ok(false) => {
                return Ok((
                    FileOutcome::Refused,
                    pc_core::tr!(
                        "пиксели больше не совпадают с сохраняемым файлом",
                        "the pixels no longer match the file being kept"
                    )
                    .into(),
                ))
            }
            Err(e) => {
                return Ok((
                    FileOutcome::Refused,
                    pc_core::tf!("проверка не удалась: {0}", "the check failed: {0}", e),
                ))
            }
        }
    }

    let file = db.file(c.file_id)?.with_context(|| {
        pc_core::tf!(
            "файл {0} исчез из индекса",
            "file {0} vanished from the index",
            c.file_id
        )
    })?;
    let dst = crate::quarantine_dest_for(&file.path, c.file_id, db, override_root)?;

    let dst_str = dst.to_string_lossy().into_owned();

    // Sidecars follow their photograph, or they become litter pointing at
    // nothing. Where each of them lands is decided here, before anything
    // moves, so the journal can say what this operation is about to do.
    let mut planned = vec![pc_db::Moved {
        src: c.path.clone(),
        dst: dst_str.clone(),
    }];
    let mut bytes = c.size;
    for side in companions(src) {
        let Some(name) = side.file_name() else {
            continue;
        };
        let target = dst.with_file_name(name);
        bytes += side.metadata().map(|m| m.len()).unwrap_or(0) as i64;
        planned.push(pc_db::Moved {
            src: side.to_string_lossy().into_owned(),
            dst: target.to_string_lossy().into_owned(),
        });
    }

    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "quarantine-file",
        target_id: Some(c.file_id),
        src: &c.path,
        dst: Some(&dst_str),
        size: bytes,
        file_count: planned.len() as i64,
        manifest: &planned,
    })?;

    match rename_with_parents(src, &dst) {
        Ok(()) => {
            let (moved, failed) = crate::carry(&planned[1..]);
            let note = match (moved.len(), failed.as_slice()) {
                (0, []) => None,
                (n, []) => Some(pc_core::tf!(
                    "спутников перенесено: {0}",
                    "companions moved: {0}",
                    n
                )),
                (n, f) => Some(pc_core::tf!(
                    "спутников перенесено: {0}, не перенеслось: {1}",
                    "companions moved: {0}, not moved: {1}",
                    n,
                    crate::listed(f)
                )),
            };
            // The journal now says what moved, not what was meant to.
            let done: Vec<pc_db::Moved> =
                std::iter::once(planned[0].clone()).chain(moved).collect();
            db.journal_set_manifest(jid, &done)?;
            db.journal_finish(jid, JournalStatus::Done, note.as_deref())?;
            // The row must stop claiming the file is still in the archive,
            // or the planner will offer the same work again forever.
            db.set_file_state(c.file_id, "quarantined")?;
            // The photograph moved; a sidecar that stayed behind is still
            // something the run has to say out loud.
            Ok((FileOutcome::Moved, crate::listed(&failed)))
        }
        Err(e) => {
            db.journal_finish(jid, JournalStatus::Failed, Some(&e.to_string()))?;
            Ok((FileOutcome::Refused, e.to_string()))
        }
    }
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    pub totals: Totals,
    pub refused: Vec<(String, String)>,
}

pub fn apply(
    db: &Db,
    run_id: i64,
    candidates: &[Candidate],
    override_root: Option<&Path>,
) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    for c in candidates {
        match quarantine_file(db, run_id, c, override_root) {
            Ok((FileOutcome::Moved, stuck)) => {
                report.totals.bundles += 1;
                report.totals.files += 1;
                report.totals.bytes += c.size as u64;
                if !stuck.is_empty() {
                    report.refused.push((c.path.clone(), stuck));
                }
            }
            Ok((FileOutcome::Refused, why)) => report.refused.push((c.path.clone(), why)),
            Err(e) => {
                // A hard error stops the run: something is wrong beyond one
                // file, and continuing would multiply it.
                bail!("{}: {e}", c.path);
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A photograph in the index, and the manual candidate that moves it.
    fn one_manual_candidate(db: &Db, run: i64, path: &Path) -> Candidate {
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as i64;
        let file_id = db
            .upsert_file(
                &pc_db::NewFile {
                    path: path.display().to_string(),
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    size,
                    ..Default::default()
                },
                run,
            )
            .unwrap();
        Candidate {
            file_id,
            family_id: 0,
            path: path.display().to_string(),
            size,
            role: pc_family::Role::Copy,
            keeper_id: 0,
            keeper_path: String::new(),
            reason: "выбор человека".into(),
            manual: true,
            group_keeper: String::new(),
        }
    }

    #[test]
    fn an_undo_brings_back_only_what_this_move_took() {
        // A stranger's sidecar can already be sitting in quarantine: another
        // photograph with the same stem was moved there long before. Looking
        // for sidecars by name at undo time hands it to whoever asks last.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("archive");
        let quarantine = dir.join(pc_core::QUARANTINE_DIR);
        fs::create_dir_all(&quarantine).unwrap();
        let photo = dir.join("photo.bmp");
        fs::write(&photo, b"picture").unwrap();
        fs::write(quarantine.join("photo.xmp"), b"someone else's edits").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let c = one_manual_candidate(&db, run, &photo);
        assert_eq!(
            quarantine_file(&db, run, &c, None).unwrap().0,
            FileOutcome::Moved
        );
        assert!(!photo.exists());

        let entry = db.journal_quarantined(None).unwrap().pop().unwrap();
        assert_eq!(entry.manifest.len(), 1, "у файла нет спутников");
        crate::undo(&db, entry.id).unwrap();

        assert_eq!(fs::read(&photo).unwrap(), b"picture");
        assert!(
            !dir.join("photo.xmp").exists(),
            "откат унёс чужой спутник в архив"
        );
        assert_eq!(
            fs::read(quarantine.join("photo.xmp")).unwrap(),
            b"someone else's edits"
        );
    }

    #[test]
    fn a_sidecar_that_did_not_move_is_written_down_not_assumed() {
        // The rename of a sidecar used to be attempted and forgotten. If it
        // fails, the journal must say so — an undo that silently finds
        // nothing to bring back is how an edit disappears.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("archive");
        let quarantine = dir.join(pc_core::QUARANTINE_DIR);
        fs::create_dir_all(&quarantine).unwrap();
        let photo = dir.join("frame.arw");
        fs::write(&photo, b"raw").unwrap();
        fs::write(dir.join("frame.xmp"), b"my edits").unwrap();
        // Something else already occupies the sidecar's destination, and a
        // directory with a file in it will not be renamed over.
        fs::create_dir_all(quarantine.join("frame.xmp")).unwrap();
        fs::write(quarantine.join("frame.xmp").join("inside"), b"x").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let c = one_manual_candidate(&db, run, &photo);
        let (outcome, stuck) = quarantine_file(&db, run, &c, None).unwrap();
        assert_eq!(outcome, FileOutcome::Moved);
        assert!(
            stuck.contains("frame.xmp"),
            "отказ спутника молчит: {stuck:?}"
        );

        let entry = db.journal_quarantined(None).unwrap().pop().unwrap();
        assert_eq!(
            entry.manifest.len(),
            1,
            "спутник не уехал, но записан как уехавший"
        );
        assert_eq!(fs::read(dir.join("frame.xmp")).unwrap(), b"my edits");
    }

    #[test]
    fn a_sidecar_in_quarantine_belongs_to_the_run_that_put_it_there() {
        // Orphan quarantine offers to adopt or purge whatever the journal
        // does not claim. A sidecar has no journal row of its own, so without
        // the manifest it looked abandoned — and could be taken away from the
        // photograph it belongs to.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("archive");
        fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("frame.arw");
        fs::write(&photo, b"raw").unwrap();
        fs::write(dir.join("frame.xmp"), b"my edits").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let c = one_manual_candidate(&db, run, &photo);
        assert_eq!(
            quarantine_file(&db, run, &c, None).unwrap().0,
            FileOutcome::Moved
        );

        let quarantine = dir.join(pc_core::QUARANTINE_DIR);
        let stranger = quarantine.join("from-another-database.jpg");
        fs::write(&stranger, b"nobody's").unwrap();
        let seen: Vec<(String, i64, i64)> = [
            quarantine.join("frame.arw"),
            quarantine.join("frame.xmp"),
            stranger.clone(),
        ]
        .iter()
        .map(|p| (p.display().to_string(), 1, 0))
        .collect();
        db.set_quarantine_found(run, &seen).unwrap();

        let found = db.quarantine_found().unwrap();
        let known = |name: &str| {
            found
                .iter()
                .find(|f| f.path.ends_with(name))
                .unwrap_or_else(|| panic!("нет {name}"))
                .known
        };
        assert!(known("frame.arw"));
        assert!(known("frame.xmp"), "спутник объявлен ничьим");
        assert!(!known("from-another-database.jpg"));
    }

    #[test]
    fn an_old_journal_does_not_reach_into_a_new_index() {
        // A reset empties the index but keeps the journal, because the
        // journal is the only record of what left the archive. SQLite then
        // hands the same row ids out again, so an entry that still carried a
        // number would mark a file it has never seen: the purge of a long
        // gone copy made an untouched photograph vanish from every view.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("archive");
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("old.bmp");
        fs::write(&old, b"old").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let c = one_manual_candidate(&db, run, &old);
        assert_eq!(
            quarantine_file(&db, run, &c, None).unwrap().0,
            FileOutcome::Moved
        );
        let entry = db.journal_quarantined(None).unwrap().pop().unwrap();

        db.reset_index().unwrap();
        let other = dir.join("other.bmp");
        fs::write(&other, b"new").unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let new_id = one_manual_candidate(&db, run, &other).file_id;
        assert_eq!(new_id, c.file_id, "id не переиспользован — сценарий не тот");

        crate::purge_entry_controlled(&db, entry.id, &pc_core::work::Control::default()).unwrap();

        let state: String = db
            .conn
            .query_row("SELECT state FROM files WHERE id = ?1", [new_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(state, "present", "чужая запись журнала спрятала живой файл");
        assert!(other.exists(), "и байты на месте");
    }

    #[test]
    fn case_aliases_do_not_duplicate_one_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("frame.xmp"), b"metadata").unwrap();
        assert_eq!(companions(&tmp.path().join("frame.ARW")).len(), 1);
    }

    #[test]
    fn sidecars_are_found_next_to_their_photograph() {
        let tmp = tempfile::tempdir().unwrap();
        let raw = tmp.path().join("DSC01234.ARW");
        fs::write(&raw, b"raw").unwrap();
        fs::write(tmp.path().join("DSC01234.xmp"), b"x").unwrap();
        fs::write(tmp.path().join("._DSC01234.ARW"), b"a").unwrap();
        fs::write(tmp.path().join("DSC09999.xmp"), b"other").unwrap();

        let found: Vec<String> = companions(&raw)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into())
            .collect();
        assert!(found.contains(&"DSC01234.xmp".to_string()));
        assert!(found.contains(&"._DSC01234.ARW".to_string()));
        assert!(
            !found.contains(&"DSC09999.xmp".to_string()),
            "чужой сайдкар"
        );
    }

    #[test]
    fn a_file_with_no_sidecars_yields_none() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("lonely.jpg");
        fs::write(&p, b"j").unwrap();
        assert!(companions(&p).is_empty());
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn bmp(dir: &Path, name: &str, colour: [u8; 3]) -> std::path::PathBuf {
        let path = dir.join(name);
        let img = RgbImage::from_pixel(64, 48, Rgb(colour));
        image::DynamicImage::ImageRgb8(img).save(&path).unwrap();
        path
    }

    /// The fault the audit found: two frames of the same brightness and
    /// different colour reduce to one grey square, and the last check before
    /// a move was reading exactly that square.
    #[test]
    fn two_colours_of_one_brightness_are_not_one_picture() {
        let tmp = tempfile::tempdir().unwrap();
        // Equal luma by the usual weights: 0.299r + 0.587g + 0.114b.
        let red = bmp(tmp.path(), "red.bmp", [200, 46, 46]);
        let green = bmp(tmp.path(), "green.bmp", [16, 92, 46]);
        assert!(
            !same_picture(&red, &green).unwrap(),
            "разные снимки признаны одним"
        );
    }

    #[test]
    fn the_same_picture_still_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = bmp(tmp.path(), "a.bmp", [10, 120, 200]);
        let b = tmp.path().join("b.bmp");
        std::fs::copy(&a, &b).unwrap();
        assert!(same_picture(&a, &b).unwrap());
    }

    #[test]
    fn a_smaller_version_is_not_the_original() {
        let tmp = tempfile::tempdir().unwrap();
        let big = bmp(tmp.path(), "big.bmp", [30, 60, 90]);
        let small = tmp.path().join("small.bmp");
        let img =
            image::open(&big)
                .unwrap()
                .resize_exact(32, 24, image::imageops::FilterType::Triangle);
        img.save(&small).unwrap();
        assert!(!same_picture(&big, &small).unwrap());
    }
}
