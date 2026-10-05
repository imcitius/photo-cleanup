//! Quarantining photographs, as opposed to regenerable caches.
//!
//! The bar here is much higher than for a Lightroom preview. A preview costs
//! time to rebuild; a photograph cannot be rebuilt at all. So nothing moves
//! until the tool has re-read the pixels and confirmed, at that moment, that
//! the frame survives somewhere else.

use anyhow::{Context, Result};
use pc_db::{Db, JournalStatus};
use pc_family::plan::Candidate;
use std::fs;
use std::path::{Path, PathBuf};

use crate::Tally;

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

/// One sidecar of a photograph about to move: where it lands, and whether
/// that place is already taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Companion {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub size: u64,
    /// Something already bears the destination (a dangling link included).
    pub taken: bool,
}

/// Where each sidecar of `src` goes when the photograph goes to `dst`.
///
/// The one policy for both interfaces (el-5vue3 D9): a sidecar whose place
/// is taken does not stop its photograph. The photograph moves; the sidecar
/// stays where it is, is never moved over what is there, and the refusal is
/// recorded in the journal and reported — by the command line after the
/// run, by the web in the preview's warnings and in the job. A renamed
/// photograph (`DSC01234_2.ARW`) takes its sidecars' names with it.
pub fn companion_plan(src: &Path, dst: &Path) -> Vec<Companion> {
    let stem = |p: &Path| {
        p.file_name()
            .and_then(|s| s.to_str())
            .map(crate::organize::stem_of)
            .unwrap_or_default()
            .to_string()
    };
    let (old_stem, new_stem) = (stem(src), stem(dst));
    companions(src)
        .into_iter()
        .filter_map(|side| {
            let name = side.file_name()?.to_str()?.to_string();
            let target =
                dst.with_file_name(crate::organize::sidecar_name(&name, &old_stem, &new_stem));
            Some(Companion {
                size: fs::symlink_metadata(&side).map(|m| m.len()).unwrap_or(0),
                taken: fs::symlink_metadata(&target).is_ok(),
                src: side,
                dst: target,
            })
        })
        .collect()
}

/// Confirm that two files really do hold the same picture.
///
/// This re-reads and re-derives the pixel hash rather than trusting what the
/// index recorded. The index may be hours old; this is the last moment before
/// something becomes hard to undo, and it is the moment worth paying for.
pub fn same_picture(a: &Path, b: &Path) -> Result<bool> {
    let open = |p: &Path| {
        fs::File::open(p)
            .with_context(|| pc_core::tf!("нет файла {0}", "no such file: {0}", p.display()))
    };
    same_picture_in((a, &open(a)?), (b, &open(b)?))
}

/// [`same_picture`] of two files already open: the ones a move is bound to
/// (el-3wizg), so what is compared is what moves. The paths only name them.
pub(crate) fn same_picture_in(a: (&Path, &fs::File), b: (&Path, &fs::File)) -> Result<bool> {
    let read = |(p, f): (&Path, &fs::File)| -> Result<(pc_image::Probe, bool)> {
        let size = f.metadata()?.len();
        let r = pc_image::read_for_probe_from(&mut &*f, size)?;
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
        return same_bytes(a.1, b.1);
    }
    Ok(pa.content_hash == pb.content_hash)
}

/// Whole files, compared as they are. Sizes first, because a difference there
/// is free to find and ends the question.
fn same_bytes(a: &fs::File, b: &fs::File) -> Result<bool> {
    use std::io::{Seek, SeekFrom};
    let (ma, mb) = (a.metadata()?, b.metadata()?);
    if ma.len() != mb.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (a, b);
    fa.seek(SeekFrom::Start(0))?;
    fb.seek(SeekFrom::Start(0))?;
    let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let read_a = fill(&mut fa, &mut ba)?;
        let read_b = fill(&mut fb, &mut bb)?;
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

/// Read until `buf` is full or the file ends: two reads of one length can
/// return different counts, which is not a difference in content.
fn fill(f: &mut impl std::io::Read, buf: &mut [u8]) -> Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        match f.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    Ok(got)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    Moved,
    /// Verification did not hold; nothing was touched.
    Refused,
}

/// What one photograph's quarantine did.
#[derive(Debug, Clone)]
pub struct Filed {
    pub outcome: FileOutcome,
    /// Why it was refused; or, when it moved, which of its sidecars did not
    /// follow and why (`path — why; …`). Empty when everything moved.
    pub why: String,
    /// What actually moved: the frame and the sidecars that followed it.
    pub done: Tally,
    /// Every object not simply where the record says, with its proven
    /// place (el-lvtmk R1).
    pub placed: Vec<crate::Placed>,
    /// A folder of the run was moved while it ran: the run stops here
    /// (el-lvtmk D2). See [`Filed::stop_error`].
    pub stop: Option<crate::FolderMoved>,
}

impl Filed {
    fn refused(why: impl Into<String>) -> Self {
        Self {
            outcome: FileOutcome::Refused,
            why: why.into(),
            done: Tally::default(),
            placed: Vec::new(),
            stop: None,
        }
    }

    /// The error that ends the run, when this photograph's move asks for a
    /// stop: what it did, and where everything is, typed — the same value
    /// the command line and the web render.
    pub fn stop_error(&self) -> Option<anyhow::Error> {
        let stop = self.stop.clone()?;
        let refused = if self.why.is_empty() {
            Vec::new()
        } else {
            vec![self.why.clone()]
        };
        let e = crate::stop_run(stop.into(), &self.done, crate::Route::Quarantine, refused);
        Some(crate::outcome::with_placed(
            e,
            self.placed.clone(),
            crate::Route::Quarantine,
        ))
    }
}

/// Evidence of the file at `path`, taken before it moves.
pub(crate) fn evidence(path: &Path) -> Result<Option<pc_core::proof::Proof>> {
    let md = fs::symlink_metadata(path)
        .with_context(|| pc_core::tf!("не прочитать {0}", "cannot read {0}", path.display()))?;
    Ok(pc_core::proof::Proof::of(&md))
}

/// Bytes of the moved files, from their evidence: what moved, not what was
/// planned (el-5vue3 D7).
pub(crate) fn moved_bytes(moved: &[pc_db::Moved]) -> u64 {
    moved
        .iter()
        .filter_map(|m| m.proof.as_ref().and_then(|p| p.size))
        .sum()
}

pub fn quarantine_file(
    db: &Db,
    run_id: i64,
    c: &Candidate,
    override_root: Option<&Path>,
) -> Result<Filed> {
    let src = Path::new(&c.path);
    let keeper = Path::new(&c.keeper_path);

    // The photograph is opened before it is checked and stays open until it
    // moves: the check reads this object, and the move is bound to it — not
    // to whatever bears the name by then (el-3wizg).
    let held = match crate::bound::Held::open(src) {
        Ok(Some(held)) => held,
        Ok(None) => {
            return Ok(Filed::refused(pc_core::tr!(
                "файла уже нет",
                "the file is already gone"
            )))
        }
        Err(e) => return Ok(Filed::refused(format!("{e:#}"))),
    };

    // A candidate the tool picked has to prove itself: the file that makes it
    // redundant must still exist and still hold the same pixels. A candidate
    // the *user* picked has no such twin and needs none — the justification is
    // that they looked at the frame and did not want it.
    let mut kept = None;
    if !c.manual {
        let keeper_held = match crate::bound::Held::open(keeper) {
            Ok(Some(k)) => k,
            Ok(None) => {
                return Ok(Filed::refused(pc_core::tf!(
                    "нет файла, ради которого удаляем: {0}",
                    "the file this one is redundant to is missing: {0}",
                    c.keeper_path
                )))
            }
            Err(e) => {
                return Ok(Filed::refused(pc_core::tf!(
                    "проверка не удалась: {0}",
                    "the check failed: {0}",
                    format!("{e:#}")
                )))
            }
        };
        if src == keeper {
            return Ok(Filed::refused(pc_core::tr!(
                "это и есть сохраняемый файл",
                "this is the file being kept"
            )));
        }
        // Re-read both, through the files held, and compare the pixels as
        // they are right now.
        match held
            .file()
            .and_then(|f| Ok((f, keeper_held.file()?)))
            .and_then(|(f, k)| same_picture_in((src, f), (keeper, k)))
        {
            Ok(true) => {}
            Ok(false) => {
                return Ok(Filed::refused(pc_core::tr!(
                    "пиксели больше не совпадают с сохраняемым файлом",
                    "the pixels no longer match the file being kept"
                )))
            }
            Err(e) => {
                return Ok(Filed::refused(pc_core::tf!(
                    "проверка не удалась: {0}",
                    "the check failed: {0}",
                    e
                )))
            }
        }
        // The keeper is held too, and asked again right before the move.
        kept = Some(keeper_held);
    }

    let file = db.file(c.file_id)?.with_context(|| {
        pc_core::tf!(
            "файл {0} исчез из индекса",
            "file {0} vanished from the index",
            c.file_id
        )
    })?;
    let target = crate::quarantine_target_for(&file.path, override_root)?;
    // The file and its keeper have passed; now the volume, and only then the
    // gathered quarantine's note — before the journal, so a refusal here
    // leaves no row behind, and anything the note had to leave is named.
    crate::admit(&target)?;
    let dst = target.dst;

    let dst_str = dst.to_string_lossy().into_owned();

    // Sidecars follow their photograph, or they become litter pointing at
    // nothing. Where each of them lands is decided here, before anything
    // moves, and so is the evidence of which file each one is: the journal
    // holds both before the first rename, so an interrupted run can be
    // recovered on that evidence and not on names.
    let mut planned = vec![pc_db::Moved {
        src: c.path.clone(),
        dst: dst_str.clone(),
        proof: Some(held.proof().clone()),
    }];
    // Every companion goes with the frame or none does (user decision (c)):
    // one whose evidence cannot be read, or whose place in quarantine is
    // taken, refuses the frame too, before anything moves.
    for side in companion_plan(src, &dst) {
        let refuse = |why: String| {
            Ok(Filed::refused(pc_core::tf!(
                "спутник {0}: {1}; кадр со спутниками не переносится, ничего не перенесено",
                "companion {0}: {1}; the frame and its companions are not moved, nothing moved",
                side.src.display(),
                why
            )))
        };
        if side.taken {
            return refuse(pc_core::tf!(
                "его место в карантине занято: {0}",
                "its place in quarantine is taken: {0}",
                side.dst.display()
            ));
        }
        let proof = match evidence(&side.src) {
            Ok(Some(p)) => p,
            Ok(None) => {
                return refuse(
                    pc_core::tr!(
                        "эта система не умеет назвать объект файловой системы",
                        "this system cannot name a file system object"
                    )
                    .to_string(),
                )
            }
            Err(e) => return refuse(format!("{e:#}")),
        };
        planned.push(pc_db::Moved {
            src: side.src.to_string_lossy().into_owned(),
            dst: side.dst.to_string_lossy().into_owned(),
            proof: Some(proof),
        });
    }

    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "quarantine-file",
        target_id: Some(c.file_id),
        src: &c.path,
        dst: Some(&dst_str),
        size: moved_bytes(&planned) as i64,
        file_count: planned.len() as i64,
        manifest: &planned,
    })?;

    let keepers: Vec<&crate::bound::Held> = kept.iter().collect();
    let members: Vec<crate::unit::Member> =
        planned.iter().map(crate::unit::Member::forward).collect();
    let unit = crate::unit::move_unit(&members, crate::located::Way::Forward, Some(held), &keepers);
    let crate::unit::Unit::Moved(arrived) = unit else {
        let r = crate::unit::close_forward(db, jid, "forward", crate::Route::Quarantine, unit)?;
        return Ok(Filed {
            placed: r.placed,
            stop: r.stop,
            ..Filed::refused(r.why)
        });
    };
    let done = Tally {
        frames: 1,
        companions: planned.len() as u64 - 1,
        bytes: moved_bytes(&planned),
        ..Default::default()
    };
    let note = match done.companions {
        0 => String::new(),
        n => pc_core::tf!("спутников перенесено: {0}", "companions moved: {0}", n),
    };
    let mut closed = false;
    let persisted = (|| -> Result<()> {
        db.journal_close(
            jid,
            JournalStatus::Done,
            &pc_db::Event {
                text: &note,
                moved: &planned,
                ..pc_db::Event::new("forward", "done")
            },
        )?;
        closed = true;
        db.set_file_state(c.file_id, "quarantined")?;
        Ok(())
    })();
    // Held until recorded.
    drop(arrived);
    if let Err(e) = persisted {
        if closed {
            let e = e.context(pc_core::tf!(
                "файл перенесён в {0} и записан в журнал, но индекс не обновлён",
                "the file moved to {0} and is in the journal, but the index did not follow",
                dst.display()
            ));
            return Err(crate::stop_run(
                e,
                &done,
                crate::Route::Quarantine,
                Vec::new(),
            ));
        }
        let e = e.context(pc_core::tf!(
            "файл перенесён в {0}, но журнал не дописан: запись {1} осталась незавершённой — сверьте её",
            "the file moved to {0}, but the journal was not completed: entry {1} is left pending — reconcile it",
            dst.display(),
            jid
        ));
        let e = crate::stop_run(e, &done, crate::Route::Quarantine, Vec::new());
        return Err(crate::outcome::left_pending(
            e,
            jid,
            crate::Route::Quarantine,
        ));
    }
    Ok(Filed {
        outcome: FileOutcome::Moved,
        why: String::new(),
        done,
        placed: Vec::new(),
        stop: None,
    })
}

#[derive(Debug, Default)]
pub struct ApplyReport {
    /// What actually moved.
    pub done: Tally,
    pub refused: Vec<(String, String)>,
    /// Every object not simply where the record says, with its proven
    /// place: the same [`crate::Placed`] the web renders.
    pub placed: Vec<crate::Placed>,
    /// The run stopped early because a folder of it was moved
    /// (el-lvtmk D2); the candidates after it are in `refused` as not tried.
    pub stopped: Option<crate::FolderMoved>,
}

impl ApplyReport {
    /// The error that ends the run, if it stopped: what moved before it and
    /// where everything is, typed.
    pub fn stop_error(&self) -> Option<anyhow::Error> {
        let stop = self.stopped.clone()?;
        let refused = self
            .refused
            .iter()
            .map(|(p, why)| format!("{p} — {why}"))
            .collect();
        let e = crate::stop_run(stop.into(), &self.done, crate::Route::Quarantine, refused);
        Some(crate::outcome::with_placed(
            e,
            self.placed.clone(),
            crate::Route::Quarantine,
        ))
    }
}

pub fn apply(
    db: &Db,
    run_id: i64,
    candidates: &[Candidate],
    override_root: Option<&Path>,
) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    // A volume that cannot move without replacing stops the run before its
    // first move.
    crate::check_candidates(db, candidates, override_root)?;
    let roots = crate::RunRoots::hold(db, candidates.iter().map(|c| c.path.as_str()))?;
    let mut todo = candidates.iter();
    while let Some(c) = todo.next() {
        if let Err(stop) = roots.check() {
            report.refused.push((c.path.clone(), not_tried_folder()));
            for left in todo.by_ref() {
                report.refused.push((left.path.clone(), not_tried_folder()));
            }
            report.stopped = Some(stop);
            break;
        }
        match quarantine_file(db, run_id, c, override_root) {
            Ok(filed) => {
                report.done.add(&filed.done);
                report.placed.extend(filed.placed);
                if !filed.why.is_empty() {
                    report.refused.push((c.path.clone(), filed.why));
                }
                if let Some(stop) = filed.stop {
                    // A folder of the run was moved: nothing more is tried,
                    // and what was not is said (el-lvtmk D2).
                    for left in todo.by_ref() {
                        report.refused.push((left.path.clone(), not_tried_folder()));
                    }
                    report.stopped = Some(stop);
                    break;
                }
            }
            // Whatever stopped it — the volume, or anything else after
            // earlier moves — the caller hears what had moved, the current
            // photograph's own share included exactly once.
            Err(e) => {
                let refused = report
                    .refused
                    .iter()
                    .map(|(p, why)| format!("{p} — {why}"))
                    .collect();
                // A hard error stops the run: something is wrong beyond one
                // file, and continuing would multiply it.
                let e = if crate::is_no_exclusive_rename(&e) || crate::stopped_run(&e).is_some() {
                    e
                } else {
                    e.context(c.path.clone())
                };
                return Err(crate::stop_run(
                    e,
                    &report.done,
                    crate::Route::Quarantine,
                    refused,
                ));
            }
        }
    }
    Ok(report)
}

/// Why a photograph the run never reached did not move.
pub(crate) fn not_tried_folder() -> String {
    pc_core::tr!(
        "не перенесён: прогон остановлен, папку прогона переместили во время работы",
        "not moved: the run stopped, a folder of it was moved while it ran"
    )
    .to_string()
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
            quarantine_file(&db, run, &c, None).unwrap().outcome,
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
    fn a_sidecar_that_cannot_move_keeps_its_frame_home_and_says_why() {
        // The sidecar's place in quarantine is taken. It used to be left
        // behind and the frame moved without it; now the frame and its
        // companions are one unit (user decision (c)): neither moves, and
        // the refusal names the sidecar.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("archive");
        let quarantine = dir.join(pc_core::QUARANTINE_DIR);
        fs::create_dir_all(&quarantine).unwrap();
        let photo = dir.join("frame.arw");
        fs::write(&photo, b"raw").unwrap();
        fs::write(dir.join("frame.xmp"), b"my edits").unwrap();
        fs::create_dir_all(quarantine.join("frame.xmp")).unwrap();
        fs::write(quarantine.join("frame.xmp").join("inside"), b"x").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[dir.display().to_string()], "test").unwrap();
        let c = one_manual_candidate(&db, run, &photo);
        let filed = quarantine_file(&db, run, &c, None).unwrap();
        assert_eq!(filed.outcome, FileOutcome::Refused);
        assert!(filed.why.contains("frame.xmp"), "{:?}", filed.why);
        assert!(filed.done.is_empty());
        assert_eq!(fs::read(&photo).unwrap(), b"raw");
        assert_eq!(fs::read(dir.join("frame.xmp")).unwrap(), b"my edits");
        assert!(!quarantine.join("frame.arw").exists());
        assert_eq!(
            fs::read(quarantine.join("frame.xmp").join("inside")).unwrap(),
            b"x"
        );
        assert!(db.journal_quarantined(None).unwrap().is_empty());
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
            quarantine_file(&db, run, &c, None).unwrap().outcome,
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
            quarantine_file(&db, run, &c, None).unwrap().outcome,
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
