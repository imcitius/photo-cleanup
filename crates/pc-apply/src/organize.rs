//! Carrying out a reorganisation.
//!
//! Every file moves by `rename(2)` within one filesystem, never replacing
//! what is at the destination (el-usdqi), with both paths in
//! the journal before the call and the index updated after it. Nothing is
//! copied, nothing is deleted, and the whole run can be walked backwards —
//! which is the only reason it is safe to rearrange an archive at all.

use anyhow::{bail, Context, Result};
use pc_db::{Db, JournalStatus};
use pc_organize::Move;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::files::{companion_plan, evidence, moved_bytes};
use crate::located::Way;
use crate::outcome::left_pending;
use crate::{Route, Tally};

#[derive(Debug, Default)]
pub struct OrganizeReport {
    /// What actually moved: photographs, the sidecars that followed them,
    /// and service files carried into quarantine out of folders the run
    /// emptied (not deleted: quarantined, and undone with the run).
    pub done: Tally,
    /// Every file not moved, every sidecar left behind — `(path, why)`.
    /// Folders the run emptied are not in it: they are simply left where
    /// they are (el-1y8uo B1, nothing is ever removed).
    pub refused: Vec<(String, String)>,
    /// Every object not simply where the record says, with its proven
    /// place (el-lvtmk R1).
    pub placed: Vec<crate::Placed>,
    /// The run stopped early because a folder of it was moved
    /// (el-lvtmk D2); the files after it are in `refused` as not tried.
    pub stopped: Option<crate::FolderMoved>,
}

impl OrganizeReport {
    /// The error that ends the run, if it stopped: what moved before it and
    /// where everything is, typed.
    pub fn stop_error(&self, run_id: i64) -> Option<anyhow::Error> {
        let stop = self.stopped.clone()?;
        Some(crate::outcome::with_placed(
            stopped(stop.into(), self, run_id),
            self.placed.clone(),
            Route::Organize { run_id },
        ))
    }
}

pub(crate) fn stem_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

/// A sidecar's new name when its photograph was renamed out of a collision:
/// `DSC01234.xmp` beside `DSC01234_2.ARW` becomes `DSC01234_2.xmp`.
pub(crate) fn sidecar_name(side: &str, old_stem: &str, new_stem: &str) -> String {
    if old_stem == new_stem {
        return side.to_string();
    }
    match side.strip_prefix(old_stem) {
        Some(rest) => format!("{new_stem}{rest}"),
        // AppleDouble: `._DSC01234.ARW`.
        None => match side
            .strip_prefix("._")
            .and_then(|r| r.strip_prefix(old_stem))
        {
            Some(rest) => format!("._{new_stem}{rest}"),
            None => side.to_string(),
        },
    }
}

/// Refuse to move a file that changed since it was indexed: whatever is on
/// disk now is not what the plan reasoned about.
fn unchanged(path: &Path, size: i64, mtime: i64) -> bool {
    match fs::metadata(path) {
        Ok(md) => md.len() as i64 == size && pc_core::time::mtime_unix(&md) == mtime,
        Err(_) => false,
    }
}

pub fn organize(db: &Db, run_id: i64, moves: &[Move]) -> Result<OrganizeReport> {
    let mut report = OrganizeReport::default();
    let mut source_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let route = Route::Organize { run_id };
    // A volume that cannot move without replacing stops the run before its
    // first move.
    crate::check_organize(moves)?;
    let roots: BTreeSet<PathBuf> = db.all_run_roots()?.into_iter().map(PathBuf::from).collect();

    let held = crate::RunRoots::hold(db, moves.iter().map(|m| m.src.as_str()))?;
    let mut todo = moves.iter();
    while let Some(m) = todo.next() {
        if report.stopped.is_none() {
            report.stopped = held.check().err();
        }
        if report.stopped.is_some() {
            report
                .refused
                .push((m.src.clone(), crate::files::not_tried_folder()));
            for left in todo.by_ref() {
                report
                    .refused
                    .push((left.src.clone(), crate::files::not_tried_folder()));
            }
            break;
        }
        let src = Path::new(&m.src);
        let dst = Path::new(&m.dst);

        if !src.is_file() {
            report.refused.push((
                m.src.clone(),
                pc_core::tr!("файла уже нет", "the file is already gone").into(),
            ));
            continue;
        }
        if !unchanged(src, m.size, m.mtime) {
            report.refused.push((
                m.src.clone(),
                pc_core::tr!(
                    "изменился с момента индексации — переиндексируйте",
                    "changed since indexing — index it again"
                )
                .into(),
            ));
            continue;
        }
        // An early answer only; the move itself refuses to replace anything.
        // `symlink_metadata` also sees a dangling link, which `exists` does not.
        if fs::symlink_metadata(dst).is_ok() {
            report.refused.push((
                m.src.clone(),
                pc_core::tf!("цель занята: {0}", "destination taken: {0}", m.dst),
            ));
            continue;
        }

        // Where the photograph and each of its sidecars land is settled
        // before the first rename: a collision may have renamed the file, and
        // the sidecars carry the new stem with it. So is the evidence of
        // which file each one is, for any later recovery.
        let frame_proof = match evidence(src) {
            Ok(p) => p,
            Err(e) => {
                report.refused.push((m.src.clone(), format!("{e:#}")));
                continue;
            }
        };
        let mut planned = vec![pc_db::Moved {
            src: m.src.clone(),
            dst: m.dst.clone(),
            proof: frame_proof,
        }];
        // Every sidecar goes with the photograph or none does (user decision
        // (c)): one that cannot be proven, or whose new name is taken,
        // refuses the photograph too, before anything moves.
        let mut refusal = None;
        for side in companion_plan(src, dst) {
            let proof = match evidence(&side.src) {
                Ok(Some(p)) if !side.taken => p,
                Ok(Some(_)) => {
                    refusal = Some(pc_core::tf!(
                        "спутник {0}: цель занята: {1}",
                        "companion {0}: destination taken: {1}",
                        side.src.display(),
                        side.dst.display()
                    ));
                    break;
                }
                Ok(None) => {
                    refusal = Some(pc_core::tf!(
                        "спутник {0}: эта система не умеет назвать объект файловой системы",
                        "companion {0}: this system cannot name a file system object",
                        side.src.display()
                    ));
                    break;
                }
                Err(e) => {
                    refusal = Some(pc_core::tf!(
                        "спутник {0}: {1}",
                        "companion {0}: {1}",
                        side.src.display(),
                        format!("{e:#}")
                    ));
                    break;
                }
            };
            planned.push(pc_db::Moved {
                src: side.src.to_string_lossy().into_owned(),
                dst: side.dst.to_string_lossy().into_owned(),
                proof: Some(proof),
            });
        }
        if let Some(why) = refusal {
            report.refused.push((
                m.src.clone(),
                pc_core::tf!(
                    "{0}; кадр со спутниками не переносится, ничего не перенесено",
                    "{0}; the frame and its companions are not moved, nothing moved",
                    why
                ),
            ));
            continue;
        }

        // Nothing of this file has moved yet; earlier files may have. A
        // journal that refuses the row stops the run with their receipt
        // (el-1y8uo B3).
        let jid = match db.journal_begin(&pc_db::NewJournalEntry {
            run_id,
            op: "organize",
            target_id: Some(m.file_id),
            src: &m.src,
            dst: Some(&m.dst),
            size: moved_bytes(&planned) as i64,
            file_count: planned.len() as i64,
            manifest: &planned,
        }) {
            Ok(jid) => jid,
            Err(e) => return Err(stopped(e, &report, run_id)),
        };

        let members: Vec<crate::unit::Member> =
            planned.iter().map(crate::unit::Member::forward).collect();
        let unit = crate::unit::move_unit(&members, Way::Forward, None, &[]);
        let crate::unit::Unit::Moved(arrived) = unit else {
            let r = match crate::unit::close_forward(db, jid, "forward", route, unit) {
                Ok(r) => r,
                Err(e) => return Err(stopped(e, &report, run_id)),
            };
            // One failed rename is a fact about one file; a storm of them
            // means the destination is wrong, and continuing would spread
            // the mess across the archive.
            report.refused.push((m.src.clone(), r.why));
            report.stopped = r.stop;
            report.placed.extend(r.placed);
            if report.refused.len() > 50 && report.done.frames == 0 {
                bail!(
                    "{}",
                    pc_core::tr!(
                        "слишком много отказов подряд, ничего не перенесено — остановка",
                        "too many refusals in a row and nothing moved — stopping"
                    )
                );
            }
            continue;
        };
        if let Some(parent) = src.parent() {
            source_dirs.insert(parent.to_path_buf());
        }
        let moved_with = planned.len() as u64 - 1;
        let done = Tally {
            frames: 1,
            companions: moved_with,
            bytes: moved_bytes(&planned),
            ..Default::default()
        };
        let note = match (&m.renamed_from, moved_with) {
            (Some(old), 0) => pc_core::tf!("переименован из {0}", "renamed from {0}", old),
            (Some(old), n) => pc_core::tf!(
                "переименован из {0}, спутников {1}",
                "renamed from {0}, {1} companions",
                old,
                n
            ),
            (None, 0) => String::new(),
            (None, n) => pc_core::tf!("спутников перенесено: {0}", "companions moved: {0}", n),
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
            let name = m.name().to_string();
            db.set_file_path(m.file_id, &m.dst, &name)
                .with_context(|| {
                    pc_core::tf!(
                        "файл перенесён, но индекс не обновлён: {0}",
                        "the file moved but the index did not follow: {0}",
                        m.dst
                    )
                })?;
            Ok(())
        })();
        drop(arrived);
        // Counted as soon as it moved: a journal that then refuses does not
        // make the move not have happened (el-1y8uo B3).
        report.done.add(&done);
        if let Err(e) = persisted {
            let e = stopped(e, &report, run_id);
            return Err(if closed {
                e
            } else {
                left_pending(e, jid, route)
            });
        }
    }

    if report.stopped.is_some() {
        // A folder of the run was moved: no folder is swept on the strength
        // of paths read before that.
        return Ok(report);
    }
    let mut pending = Vec::new();
    if let Err(e) = sweep_litter(db, run_id, &source_dirs, &roots, &mut report, &mut pending) {
        let mut e = stopped(e, &report, run_id);
        for id in pending {
            e = left_pending(e, id, route);
        }
        return Err(e);
    }
    Ok(report)
}

/// A reorganisation stops at `e`, and says what it had moved by then:
/// photographs, sidecars and service files alike.
fn stopped(e: anyhow::Error, report: &OrganizeReport, run_id: i64) -> anyhow::Error {
    let refused = report
        .refused
        .iter()
        .map(|(p, why)| format!("{p} — {why}"))
        .collect();
    crate::stop_run(e, &report.done, crate::Route::Organize { run_id }, refused)
}

/// Service files the system leaves behind: Finder's note about a folder, and
/// the AppleDouble half of a file that is no longer beside it.
fn is_litter(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._")
}

/// Carry the service files out of the directories the run emptied.
///
/// They are not ours to delete: an AppleDouble can hold a resource fork, and
/// this tool deletes nothing. So they move into quarantine beside the
/// directory, with a journal entry of the same run, and `organize undo`
/// brings them back with everything else. The emptied directory itself
/// stays where it is (el-1y8uo B1): nothing is ever removed.
fn sweep_litter(
    db: &Db,
    run_id: i64,
    dirs: &BTreeSet<PathBuf>,
    keep: &BTreeSet<PathBuf>,
    report: &mut OrganizeReport,
    pending: &mut Vec<i64>,
) -> Result<()> {
    for dir in dirs {
        if keep.contains(dir) {
            continue;
        }
        let Ok(rd) = fs::read_dir(dir) else { continue };
        // An entry that cannot be read means the folder's contents are not
        // known: nothing is swept out of a folder seen only in part.
        let Ok(entries) = rd.collect::<std::io::Result<Vec<_>>>() else {
            report.refused.push((
                dir.display().to_string(),
                pc_core::tr!(
                    "содержимое каталога прочитано не полностью; служебные файлы не тронуты",
                    "the folder could not be read in full; its service files were left alone"
                )
                .into(),
            ));
            continue;
        };
        // Only a directory left with nothing but service files, and only its
        // own files — a subdirectory means the reorganisation is not done here.
        let swept = !entries.is_empty()
            && entries.iter().all(|e| {
                e.file_type().is_ok_and(|t| t.is_file())
                    && e.file_name().to_str().is_some_and(is_litter)
            });
        if !swept {
            continue;
        }
        let Ok(home) = crate::beside(dir) else {
            continue;
        };
        for e in entries {
            let src = e.path();
            let dst = home.join(e.file_name());
            let src_s = src.to_string_lossy().into_owned();
            // An early answer; the move itself never replaces anything.
            if fs::symlink_metadata(&dst).is_ok() {
                report.refused.push((
                    src_s,
                    pc_core::tf!("цель занята: {0}", "destination taken: {0}", dst.display()),
                ));
                continue;
            }
            let dst_s = dst.to_string_lossy().into_owned();
            let proof = match evidence(&src) {
                Ok(p) => p,
                Err(err) => {
                    report.refused.push((src_s, format!("{err:#}")));
                    continue;
                }
            };
            let size = proof.as_ref().and_then(|p| p.size).unwrap_or(0);
            let manifest = [pc_db::Moved {
                src: src_s.clone(),
                dst: dst_s.clone(),
                proof,
            }];
            let jid = db.journal_begin(&pc_db::NewJournalEntry {
                run_id,
                op: "organize",
                target_id: None,
                src: &src_s,
                dst: Some(&dst_s),
                size: size as i64,
                file_count: 1,
                manifest: &manifest,
            })?;
            let unit = crate::unit::move_unit(
                &[crate::unit::Member::forward(&manifest[0])],
                Way::Forward,
                None,
                &[],
            );
            let crate::unit::Unit::Moved(arrived) = unit else {
                let r = match crate::unit::close_forward(
                    db,
                    jid,
                    "forward",
                    Route::Organize { run_id },
                    unit,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        if crate::stopped_run(&e).is_some_and(|s| s.pending.contains(&jid)) {
                            pending.push(jid);
                        }
                        return Err(e);
                    }
                };
                report.placed.extend(r.placed);
                report.refused.push((src_s, r.why));
                if let Some(stop) = r.stop {
                    report.stopped = Some(stop);
                    return Ok(());
                }
                continue;
            };
            // Counted as soon as it moved: a journal that then refuses does
            // not make the move not have happened (el-1y8uo B3).
            report.done.litter += 1;
            report.done.bytes += size;
            let closed = db.journal_close(
                jid,
                JournalStatus::Done,
                &pc_db::Event {
                    text: pc_core::tr!(
                        "служебный файл из опустевшего каталога",
                        "a service file from an emptied directory"
                    ),
                    moved: &manifest,
                    ..pc_db::Event::new("forward", "done")
                },
            );
            drop(arrived);
            if let Err(e) = closed {
                pending.push(jid);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Walk one reorganisation run backwards, newest move first. What came
/// back is counted from what each undo actually did — entries walked back
/// whole, entries walked back in part, and files.
pub fn undo_run(db: &Db, run_id: i64) -> Result<(Tally, Vec<String>)> {
    let entries = db.journal_by_run_op(run_id, "organize")?;
    if entries.is_empty() {
        bail!(
            "{}",
            pc_core::tf!(
                "в прогоне {0} нет перенесённых файлов",
                "run {0} moved no files",
                run_id
            )
        );
    }
    let mut back = Tally::default();
    let mut failed = Vec::new();
    for e in entries {
        match crate::undo(db, e.id) {
            Ok(t) => back.add(&t),
            // The volume cannot move without replacing: every later entry
            // would meet it too, so the walk stops here, saying how far it
            // got. Its own entry stays `done`, with the reason on it.
            // A folder of the run was moved: the same, for the same reason.
            Err(err) if crate::is_run_stop(&err) => {
                return Err(crate::stop_run(err, &back, crate::Route::Restore, failed));
            }
            Err(err) => {
                if let Some(part) = crate::stopped_run(&err) {
                    back.add(&part.done);
                }
                failed.push(format!("{} — {err:#}", e.src));
            }
        }
    }
    Ok((back, failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_undo_after_a_reset_moves_the_file_and_leaves_the_index_alone() {
        // Same reuse of row ids as in quarantine: an old reorganisation entry
        // must still put the file back on disk, and must not rewrite the path
        // of whatever now holds the number it was written with.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("foto");
        let dest = root.join("2019");
        fs::create_dir_all(&dest).unwrap();
        let src = root.join("a.jpg");
        let dst = dest.join("a.jpg");
        fs::write(&src, b"picture").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        let file_id = db
            .upsert_file(
                &pc_db::NewFile {
                    path: src.display().to_string(),
                    name: "a.jpg".into(),
                    size: 7,
                    mtime: pc_core::time::mtime_unix(&fs::metadata(&src).unwrap()),
                    ..Default::default()
                },
                run,
            )
            .unwrap();
        let report = organize(
            &db,
            run,
            &[Move {
                file_id,
                src: src.display().to_string(),
                dst: dst.display().to_string(),
                size: 7,
                mtime: pc_core::time::mtime_unix(&fs::metadata(&src).unwrap()),
                date: pc_organize::Dated {
                    ts: 1_562_000_000,
                    source: pc_organize::Source::Exif,
                    precision: pc_organize::Precision::Day,
                },
                event: "2019".into(),
                renamed_from: None,
            }],
        )
        .unwrap();
        assert_eq!(report.done.frames, 1, "{:?}", report.refused);

        db.reset_index().unwrap();
        let stranger = root.join("stranger.jpg");
        fs::write(&stranger, b"someone else").unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        let new_id = db
            .upsert_file(
                &pc_db::NewFile {
                    path: stranger.display().to_string(),
                    name: "stranger.jpg".into(),
                    ..Default::default()
                },
                run,
            )
            .unwrap();
        assert_eq!(new_id, file_id, "id не переиспользован — сценарий не тот");

        let entry = db.journal_by_run_op(1, "organize").unwrap().pop().unwrap();
        crate::undo(&db, entry.id).unwrap();

        assert!(src.exists(), "файл не вернулся");
        let path: String = db
            .conn
            .query_row("SELECT path FROM files WHERE id = ?1", [new_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            path,
            stranger.display().to_string(),
            "откат переписал путь чужому файлу"
        );
    }

    #[test]
    fn a_service_file_is_quarantined_not_deleted_and_comes_back() {
        // An AppleDouble can carry a resource fork, and Finder's folder note
        // is still the user's byte. The reorganisation used to delete both to
        // get the empty directory removed — the one promise this tool makes is
        // that it never deletes anything.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("foto");
        let dir = root.join("2019");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".DS_Store"), b"finder").unwrap();
        fs::write(dir.join("._unrelated"), b"resource fork").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        let mut report = OrganizeReport::default();
        let dirs: BTreeSet<PathBuf> = [dir.clone()].into_iter().collect();
        let keep: BTreeSet<PathBuf> = [root.clone()].into_iter().collect();
        sweep_litter(&db, run, &dirs, &keep, &mut report, &mut Vec::new()).unwrap();
        assert_eq!(report.done.litter, 2, "{:?}", report.refused);
        // The emptied folder stays: nothing is ever removed (el-1y8uo B1).
        assert!(dir.is_dir());

        let home = root.join(pc_core::QUARANTINE_DIR).join("2019");
        assert_eq!(
            fs::read(home.join("._unrelated")).unwrap(),
            b"resource fork"
        );

        // And the run walks backwards whole: the service files came in on the
        // same journal, so the undo brings them home.
        let entries = db.journal_by_run_op(run, "organize").unwrap();
        assert_eq!(entries.len(), 2);
        for e in entries {
            crate::undo(&db, e.id).unwrap();
        }
        assert_eq!(fs::read(dir.join(".DS_Store")).unwrap(), b"finder");
        assert_eq!(fs::read(dir.join("._unrelated")).unwrap(), b"resource fork");
    }

    #[test]
    fn a_directory_that_still_holds_something_of_yours_stays() {
        // Service files go only out of a directory that has nothing else left.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("2019");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".DS_Store"), b"finder").unwrap();
        fs::write(dir.join("notes.txt"), b"mine").unwrap();

        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[], "test").unwrap();
        let mut report = OrganizeReport::default();
        let dirs: BTreeSet<PathBuf> = [dir.clone()].into_iter().collect();
        sweep_litter(
            &db,
            run,
            &dirs,
            &BTreeSet::new(),
            &mut report,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(report.done.litter, 0);
        assert!(dir.join(".DS_Store").exists());
        assert!(dir.join("notes.txt").exists());
    }

    #[test]
    fn a_sidecar_follows_its_photograph_through_a_rename() {
        assert_eq!(
            sidecar_name("DSC01234.xmp", "DSC01234", "DSC01234_2"),
            "DSC01234_2.xmp"
        );
        assert_eq!(
            sidecar_name("._DSC01234.ARW", "DSC01234", "DSC01234_2"),
            "._DSC01234_2.ARW"
        );
        assert_eq!(
            sidecar_name("DSC01234.xmp", "DSC01234", "DSC01234"),
            "DSC01234.xmp"
        );
    }

    #[test]
    fn an_unrelated_name_is_left_alone() {
        assert_eq!(
            sidecar_name("notes.txt", "DSC01234", "DSC01234_2"),
            "notes.txt"
        );
    }
}
