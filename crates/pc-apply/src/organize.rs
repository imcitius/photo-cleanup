//! Carrying out a reorganisation.
//!
//! Every file moves by `rename(2)` within one filesystem, with both paths in
//! the journal before the call and the index updated after it. Nothing is
//! copied, nothing is deleted, and the whole run can be walked backwards —
//! which is the only reason it is safe to rearrange an archive at all.

use anyhow::{bail, Context, Result};
use pc_db::{Db, JournalStatus};
use pc_organize::Move;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::files::companions;
use crate::rename_with_parents;

/// `YYYY/событие`: the depth of the tree this tool builds, and therefore how
/// far up an undo may tidy behind itself. The root above that is the user's.
pub(crate) const UNDO_LEVELS: usize = 2;

#[derive(Debug, Default)]
pub struct OrganizeReport {
    pub moved: u64,
    pub bytes: u64,
    pub sidecars: u64,
    pub pruned_dirs: u64,
    /// Service files carried into quarantine out of directories the run
    /// emptied. Not deleted: quarantined, and undone with the run.
    pub litter: u64,
    pub refused: Vec<(String, String)>,
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

    for m in moves {
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
        if dst.exists() {
            report.refused.push((
                m.src.clone(),
                pc_core::tf!("цель занята: {0}", "destination taken: {0}", m.dst),
            ));
            continue;
        }

        // Where the photograph and each of its sidecars land is settled
        // before the first rename: a collision may have renamed the file, and
        // the sidecars carry the new stem with it.
        let old_stem = src
            .file_name()
            .and_then(|s| s.to_str())
            .map(stem_of)
            .unwrap_or_default()
            .to_string();
        let new_stem = stem_of(m.name()).to_string();
        let mut planned = vec![pc_db::Moved {
            src: m.src.clone(),
            dst: m.dst.clone(),
        }];
        let mut bytes = m.size;
        for side in companions(src) {
            let Some(name) = side.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let target = dst.with_file_name(sidecar_name(name, &old_stem, &new_stem));
            bytes += side.metadata().map(|md| md.len()).unwrap_or(0) as i64;
            planned.push(pc_db::Moved {
                src: side.to_string_lossy().into_owned(),
                dst: target.to_string_lossy().into_owned(),
            });
        }

        let jid = db.journal_begin(&pc_db::NewJournalEntry {
            run_id,
            op: "organize",
            target_id: Some(m.file_id),
            src: &m.src,
            dst: Some(&m.dst),
            size: bytes,
            file_count: planned.len() as i64,
            manifest: &planned,
        })?;

        match rename_with_parents(src, dst) {
            Ok(()) => {
                let (carried, failed) = crate::carry(&planned[1..]);
                let moved_with = carried.len() as u64;
                report.sidecars += moved_with;
                report.refused.extend(failed.iter().cloned());
                let done: Vec<pc_db::Moved> =
                    std::iter::once(planned[0].clone()).chain(carried).collect();
                db.journal_set_manifest(jid, &done)?;

                let note = match (&m.renamed_from, moved_with) {
                    (Some(old), 0) => {
                        Some(pc_core::tf!("переименован из {0}", "renamed from {0}", old))
                    }
                    (Some(old), n) => Some(pc_core::tf!(
                        "переименован из {0}, спутников {1}",
                        "renamed from {0}, {1} companions",
                        old,
                        n
                    )),
                    (None, 0) => None,
                    (None, n) => Some(pc_core::tf!(
                        "спутников перенесено: {0}",
                        "companions moved: {0}",
                        n
                    )),
                };
                let note = match failed.as_slice() {
                    [] => note,
                    f => Some(format!(
                        "{}{}",
                        note.map(|n| format!("{n}; ")).unwrap_or_default(),
                        pc_core::tf!("не перенеслось: {0}", "not moved: {0}", crate::listed(f))
                    )),
                };
                db.journal_finish(jid, JournalStatus::Done, note.as_deref())?;

                let name = m.name().to_string();
                db.set_file_path(m.file_id, &m.dst, &name)
                    .with_context(|| {
                        pc_core::tf!(
                            "файл перенесён, но индекс не обновлён: {0}",
                            "the file moved but the index did not follow: {0}",
                            m.dst
                        )
                    })?;

                if let Some(parent) = src.parent() {
                    source_dirs.insert(parent.to_path_buf());
                }
                report.moved += 1;
                report.bytes += m.size as u64;
            }
            Err(e) => {
                db.journal_finish(jid, JournalStatus::Failed, Some(&e.to_string()))?;
                // One failed rename is a fact about one file; a storm of them
                // means the destination is wrong, and continuing would spread
                // the mess across the archive.
                report.refused.push((m.src.clone(), e.to_string()));
                if report.refused.len() > 50 && report.moved == 0 {
                    bail!(
                        "{}",
                        pc_core::tr!(
                            "слишком много отказов подряд, ничего не перенесено — остановка",
                            "too many refusals in a row and nothing moved — stopping"
                        )
                    );
                }
            }
        }
    }

    let roots: BTreeSet<PathBuf> = db.all_run_roots()?.into_iter().map(PathBuf::from).collect();
    sweep_litter(db, run_id, &source_dirs, &roots, &mut report)?;
    report.pruned_dirs = prune_empty(&source_dirs, &roots, usize::MAX);
    Ok(report)
}

/// Service files the system leaves behind: Finder's note about a folder, and
/// the AppleDouble half of a file that is no longer beside it.
fn is_litter(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._")
}

/// Carry the service files out of the directories the run emptied.
///
/// Such a directory cannot be removed while they are in it, and they are not
/// ours to delete: an AppleDouble can hold a resource fork, and this tool
/// deletes nothing. So they move into quarantine beside the directory, with a
/// journal entry of the same run — the husk can go, and `organize undo` brings
/// them back with everything else.
fn sweep_litter(
    db: &Db,
    run_id: i64,
    dirs: &BTreeSet<PathBuf>,
    keep: &BTreeSet<PathBuf>,
    report: &mut OrganizeReport,
) -> Result<()> {
    for dir in dirs {
        if keep.contains(dir) {
            continue;
        }
        let Ok(rd) = fs::read_dir(dir) else { continue };
        let entries: Vec<_> = rd.flatten().collect();
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
            if dst.exists() {
                report.refused.push((
                    src_s,
                    pc_core::tf!("цель занята: {0}", "destination taken: {0}", dst.display()),
                ));
                continue;
            }
            let dst_s = dst.to_string_lossy().into_owned();
            let size = e.metadata().map(|m| m.len()).unwrap_or(0) as i64;
            let jid = db.journal_begin(&pc_db::NewJournalEntry {
                run_id,
                op: "organize",
                target_id: None,
                src: &src_s,
                dst: Some(&dst_s),
                size,
                file_count: 1,
                manifest: &[pc_db::Moved {
                    src: src_s.clone(),
                    dst: dst_s.clone(),
                }],
            })?;
            match rename_with_parents(&src, &dst) {
                Ok(()) => {
                    db.journal_finish(
                        jid,
                        JournalStatus::Done,
                        Some(pc_core::tr!(
                            "служебный файл из опустевшего каталога",
                            "a service file from an emptied directory"
                        )),
                    )?;
                    report.litter += 1;
                }
                Err(err) => {
                    db.journal_finish(jid, JournalStatus::Failed, Some(&err.to_string()))?;
                    report.refused.push((src_s, err.to_string()));
                }
            }
        }
    }
    Ok(())
}

/// Remove directories the reorganisation emptied, deepest first.
///
/// `remove_dir` refuses a directory that still holds anything, so this can
/// only ever take away husks — a directory with a forgotten `.DS_Store` in it
/// stays, and its service files go to quarantine in `sweep_litter` first. It stops at the mount point and at the roots
/// the archive was scanned from: an empty `foto/` is still where the archive
/// lives, and finding it gone would be alarming even though nothing was lost.
pub(crate) fn prune_empty(
    dirs: &BTreeSet<PathBuf>,
    keep: &BTreeSet<PathBuf>,
    levels: usize,
) -> u64 {
    let mut map = pc_core::DiskMap::new();
    let mut pruned = 0;
    let mut ordered: Vec<&PathBuf> = dirs.iter().collect();
    ordered.sort_by_key(|p| std::cmp::Reverse(p.components().count()));

    for dir in ordered {
        let mount = map.resolve(dir).map(|d| d.mount).unwrap_or_default();
        let mut cur = dir.clone();
        let mut climbed = 0;
        while climbed < levels && cur != mount && !keep.contains(&cur) && cur.parent().is_some() {
            if fs::remove_dir(&cur).is_err() {
                break;
            }
            pruned += 1;
            climbed += 1;
            match cur.parent() {
                Some(p) => cur = p.to_path_buf(),
                None => break,
            }
        }
    }
    pruned
}

/// Walk one reorganisation run backwards, newest move first.
pub fn undo_run(db: &Db, run_id: i64) -> Result<(u64, Vec<String>)> {
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
    let mut back = 0;
    let mut failed = Vec::new();
    for e in entries {
        match crate::undo(db, e.id) {
            Ok(()) => back += 1,
            Err(err) => failed.push(format!("{} — {err}", e.src)),
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
        assert_eq!(report.moved, 1, "{:?}", report.refused);

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
        sweep_litter(&db, run, &dirs, &keep, &mut report).unwrap();
        assert_eq!(report.litter, 2, "{:?}", report.refused);
        assert_eq!(prune_empty(&dirs, &keep, usize::MAX), 1);
        assert!(!dir.exists(), "опустевший каталог должен уйти");

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
        sweep_litter(&db, run, &dirs, &BTreeSet::new(), &mut report).unwrap();
        assert_eq!(report.litter, 0);
        assert_eq!(prune_empty(&dirs, &BTreeSet::new(), usize::MAX), 0);
        assert!(dir.join(".DS_Store").exists());
        assert!(dir.join("notes.txt").exists());
    }

    #[test]
    fn a_scanned_root_is_never_taken_away_even_when_it_empties() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("foto");
        let inner = root.join("сброс");
        fs::create_dir_all(&inner).unwrap();

        let keep: BTreeSet<PathBuf> = [root.clone()].into_iter().collect();
        let dirs: BTreeSet<PathBuf> = [inner.clone()].into_iter().collect();
        assert_eq!(prune_empty(&dirs, &keep, usize::MAX), 1);
        assert!(!inner.exists(), "опустевший подкаталог должен уйти");
        assert!(root.is_dir(), "корень архива остаётся на месте");
    }

    #[test]
    fn undo_cleans_up_only_what_the_reorganisation_built() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("Архив");
        let event = dest.join("2019/2019-07-14");
        fs::create_dir_all(&event).unwrap();

        // Two levels is exactly the tree this tool creates: the year and the
        // event inside it. The root above them is the user's.
        let pruned = prune_empty(
            &[event].into_iter().collect(),
            &BTreeSet::new(),
            UNDO_LEVELS,
        );
        assert_eq!(pruned, 2);
        assert!(
            dest.is_dir(),
            "корень нового дерева не наш, чтобы его убирать"
        );
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
