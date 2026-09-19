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
        if p.is_file() && !out.contains(&p) {
            out.push(p);
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
            .with_context(|| format!("нет файла {}", p.display()))?
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
        return Ok((FileOutcome::Refused, "файла уже нет".into()));
    }
    if !keeper.is_file() {
        return Ok((
            FileOutcome::Refused,
            format!("нет файла, ради которого удаляем: {}", c.keeper_path),
        ));
    }
    if src == keeper {
        return Ok((FileOutcome::Refused, "это и есть сохраняемый файл".into()));
    }

    // Re-read both and compare the pixels as they are right now.
    match same_picture(src, keeper) {
        Ok(true) => {}
        Ok(false) => {
            return Ok((
                FileOutcome::Refused,
                "пиксели больше не совпадают с сохраняемым файлом".into(),
            ))
        }
        Err(e) => return Ok((FileOutcome::Refused, format!("проверка не удалась: {e}"))),
    }

    let file = db
        .file(c.file_id)?
        .with_context(|| format!("файл {} исчез из индекса", c.file_id))?;
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
            let note = (moved_with > 0).then(|| format!("спутников перенесено: {moved_with}"));
            db.journal_finish(jid, JournalStatus::Done, note.as_deref())?;
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
