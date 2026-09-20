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

        let jid = db.journal_begin(&pc_db::NewJournalEntry {
            run_id,
            op: "organize",
            target_id: Some(m.file_id),
            src: &m.src,
            dst: Some(&m.dst),
            size: m.size,
            file_count: 1,
        })?;

        match rename_with_parents(src, dst) {
            Ok(()) => {
                let old_stem = src
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(stem_of)
                    .unwrap_or_default()
                    .to_string();
                let new_stem = stem_of(m.name()).to_string();

                let mut moved_with = 0;
                for side in companions(src) {
                    let Some(name) = side.file_name().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let target = dst.with_file_name(sidecar_name(name, &old_stem, &new_stem));
                    if rename_with_parents(&side, &target).is_ok() {
                        moved_with += 1;
                    }
                }
                report.sidecars += moved_with;

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
    report.pruned_dirs = prune_empty(&source_dirs, &roots, usize::MAX);
    Ok(report)
}

/// Remove directories the reorganisation emptied, deepest first.
///
/// `remove_dir` refuses a directory that still holds anything, so this can
/// only ever take away husks. It stops at the mount point and at the roots
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
            // An AppleDouble or a .DS_Store left alone would keep an
            // otherwise empty directory alive forever; nothing else is
            // removed, and only inside a directory we just emptied.
            let only_litter = fs::read_dir(&cur).ok().is_some_and(|rd| {
                let entries: Vec<_> = rd.flatten().collect();
                !entries.is_empty()
                    && entries.iter().all(|e| {
                        let n = e.file_name();
                        let n = n.to_string_lossy();
                        n == ".DS_Store" || n.starts_with("._")
                    })
            });
            if only_litter {
                if let Ok(rd) = fs::read_dir(&cur) {
                    for e in rd.flatten() {
                        let _ = fs::remove_file(e.path());
                    }
                }
            }
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
