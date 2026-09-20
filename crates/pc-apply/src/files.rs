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
    let hash = |p: &Path| -> Result<[u8; 32]> {
        let size = fs::metadata(p)
            .with_context(|| pc_core::tf!("нет файла {0}", "no such file: {0}", p.display()))?
            .len();
        let r = pc_image::read_for_probe(p, size)?;
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let probe = pc_image::probe_parts(p, &r.head, r.preview.as_deref(), name)?;
        Ok(pc_hash::pixel_hash(&probe.thumb.gray))
    };
    Ok(hash(a)? == hash(b)?)
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
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "quarantine-file",
        target_id: Some(c.file_id),
        src: &c.path,
        dst: Some(&dst_str),
        size: c.size,
        file_count: 1,
    })?;

    match rename_with_parents(src, &dst) {
        Ok(()) => {
            // Sidecars follow their photograph, or they become litter
            // pointing at nothing.
            let mut moved_with = 0;
            for side in companions(src) {
                let Some(name) = side.file_name() else {
                    continue;
                };
                let target = dst.with_file_name(name);
                if rename_with_parents(&side, &target).is_ok() {
                    moved_with += 1;
                }
            }
            let note = (moved_with > 0).then(|| {
                pc_core::tf!(
                    "спутников перенесено: {0}",
                    "companions moved: {0}",
                    moved_with
                )
            });
            db.journal_finish(jid, JournalStatus::Done, note.as_deref())?;
            // The row must stop claiming the file is still in the archive,
            // or the planner will offer the same work again forever.
            db.set_file_state(c.file_id, "quarantined")?;
            Ok((FileOutcome::Moved, String::new()))
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
            Ok((FileOutcome::Moved, _)) => {
                report.totals.bundles += 1;
                report.totals.files += 1;
                report.totals.bytes += c.size as u64;
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
