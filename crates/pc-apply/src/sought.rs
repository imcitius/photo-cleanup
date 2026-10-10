//! Finding a quarantined object whose recorded place leads nowhere, by its
//! evidence (el-6byp2; diagnosis el-lvtmk D1, D3, R8).
//!
//! The default quarantine sits inside the photo's own folder. A user who
//! renames that folder after `apply` takes the quarantine along, and every
//! path the journal recorded for the entry leads nowhere: the undo used to
//! say "neither at its source nor in quarantine" about a photograph that was
//! one folder away. An interrupted run whose quarantine moved before the
//! reconciliation (#15) is the same case for a `pending` row.
//!
//! The search is deliberately narrow. Each limit is what keeps "found
//! somewhere" from turning into "taken from somewhere":
//!
//! * **Read only.** Looking moves nothing and writes nothing; it runs for
//!   every preview.
//! * **Bounded.** Only the tool's own `.photo-cleanup-quarantine` folders
//!   are read: those found under the scanned roots that hold the recorded
//!   paths, and the recorded quarantine folders themselves where they are
//!   still folders. Never the rest of the archive, never a whole volume;
//!   links are not followed, another device is not entered (a rename never
//!   crosses one), and a `.photoslibrary` is never entered at all.
//! * **By evidence, never by name.** Device and inode first, then the
//!   whole recorded evidence — size, modification and birth time, the
//!   journaled hash — through the open file. A file of the same name and
//!   size is a stranger. A row without evidence is not searched at all.
//! * **One answer or none.** Two names for the object, or a match whose
//!   evidence no longer holds, is shown and left.
//! * **A frame alone.** The tool brings a found object back itself only
//!   when the entry is one file with no companion — recorded, beside the
//!   found file, or at home. Anything else is shown, with every place
//!   found, for a person to return by hand. A companion is exactly where
//!   the earlier undo rounds lost halves (el-14vx0).
//! * **Bound to the preview.** The place found is what the preview shows;
//!   the action is given that place and searches again. A different
//!   answer — moved again, changed, a companion appeared — refuses the
//!   whole entry before anything moves, and the object is proven once more
//!   through its held folder right before its rename (`unit::move_unit`).
//!
//! Every answer is appended to the entry's history as a `sought` record
//! ([`pc_db::Sought`]), never as a `located` overlay: a place found here
//! must not become a place a later recovery acts on without a preview.

use anyhow::Result;
use pc_core::proof::{Kind, Proof, Verdict};
use pc_db::{Db, Moved, ReportOnly, Sought, SoughtFinding};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// The last `.photo-cleanup-quarantine` folder on `path`, if it has one.
pub(crate) fn quarantine_of(path: &Path) -> Option<PathBuf> {
    let mut at = PathBuf::new();
    let mut found = None;
    for c in path.components() {
        at.push(c);
        if c.as_os_str() == pc_core::QUARANTINE_DIR {
            found = Some(at.clone());
        }
    }
    found
}

/// A folder of its own at `p`, not a link to one.
fn real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// `p` as the system spells it, so that one file reached by two spellings
/// (`/var` and `/private/var`) is one place, not two.
fn spelled(p: &Path) -> PathBuf {
    match (p.parent(), p.file_name()) {
        (Some(dir), Some(name)) => fs::canonicalize(dir)
            .map(|d| d.join(name))
            .unwrap_or_else(|_| p.to_path_buf()),
        _ => p.to_path_buf(),
    }
}

#[cfg(unix)]
fn dev_of(md: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.dev()
}

#[cfg(unix)]
fn ident_of(md: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (md.dev(), md.ino())
}

/// The quarantine folders under `root`, on device `dev`.
#[cfg(unix)]
fn quarantines_under(root: &Path, dev: u64, out: &mut Vec<PathBuf>) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            // Not following a link: `file_type` and `metadata` of an entry
            // describe the entry itself. Only folders are asked further, so
            // the archive's files are listed, never opened or stat'ed.
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            if !md.is_dir() || dev_of(&md) != dev {
                continue;
            }
            let name = e.file_name();
            if name == pc_core::QUARANTINE_DIR {
                out.push(e.path());
            } else if !name
                .to_string_lossy()
                .to_lowercase()
                .ends_with(".photoslibrary")
            {
                stack.push(e.path());
            }
        }
    }
}

/// Every regular file in the quarantine folder `tree` that is the object
/// `(dev, ino)`.
#[cfg(unix)]
fn named_in(tree: &Path, want: (u64, u64), out: &mut BTreeSet<PathBuf>) {
    let mut stack = vec![tree.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() && dev_of(&md) == want.0 {
                stack.push(e.path());
            } else if md.is_file() && ident_of(&md) == want {
                out.insert(spelled(&e.path()));
            }
        }
    }
}

/// The quarantine folders to read for `rec`, as the system spells them.
fn scope(db: &Db, rec: &Moved, look: &Moved, dev: u64) -> Result<Vec<PathBuf>> {
    let recorded = [
        Path::new(&rec.src),
        Path::new(&rec.dst),
        Path::new(&look.dst),
    ];
    let mut roots: Vec<PathBuf> = db
        .all_run_roots()?
        .into_iter()
        .map(PathBuf::from)
        .filter(|r| recorded.iter().any(|p| p.starts_with(r)))
        .collect();
    roots.sort();
    roots.dedup();
    let mut trees = Vec::new();
    #[cfg(unix)]
    for r in &roots {
        quarantines_under(r, dev, &mut trees);
    }
    #[cfg(not(unix))]
    let _ = dev;
    for anchor in [&rec.dst, &look.dst] {
        if let Some(q) = quarantine_of(Path::new(anchor)).filter(|q| real_dir(q)) {
            trees.push(q);
        }
    }
    let mut seen = BTreeSet::new();
    Ok(trees
        .into_iter()
        .map(|t| fs::canonicalize(&t).unwrap_or(t))
        .filter(|t| seen.insert(t.clone()))
        .collect())
}

/// What the evidence `p` finds in `trees`.
fn finding(p: &Proof, trees: &[PathBuf]) -> SoughtFinding {
    if p.kind != Kind::File {
        // Regular files only: a folder moved whole is not looked for.
        return SoughtFinding::NotFound;
    }
    let mut hits = BTreeSet::new();
    #[cfg(unix)]
    for t in trees {
        named_in(t, (p.dev, p.ino), &mut hits);
    }
    #[cfg(not(unix))]
    let _ = trees;
    let mut hits: Vec<PathBuf> = hits.into_iter().collect();
    match hits.len() {
        0 => SoughtFinding::NotFound,
        1 => {
            let at = hits.pop().unwrap_or_default();
            let shown = at.display().to_string();
            let verdict = match fs::symlink_metadata(&at) {
                Ok(md) => crate::recovery::verdict_at(&shown, p, &md),
                Err(_) => Verdict::Unprovable("it can no longer be read"),
            };
            match verdict {
                Verdict::Same => SoughtFinding::Found { at: shown },
                Verdict::Differs(why) | Verdict::Unprovable(why) => SoughtFinding::Changed {
                    at: shown,
                    why: why.to_string(),
                },
            }
        }
        _ => SoughtFinding::Ambiguous {
            at: hits.iter().map(|h| h.display().to_string()).collect(),
        },
    }
}

/// Look for the item `rec` (looked for at `look`), a member of a unit of
/// `members` items. Reads only.
pub(crate) fn seek(db: &Db, rec: &Moved, look: &Moved, members: usize) -> Result<Sought> {
    let Some(p) = &look.proof else {
        return Ok(Sought {
            src: rec.src.clone(),
            dst: rec.dst.clone(),
            scope: Vec::new(),
            finding: SoughtFinding::NoEvidence,
            report_only: Some(ReportOnly::NoEvidence),
        });
    };
    let trees = scope(db, rec, look, p.dev)?;
    let finding = finding(p, &trees);
    let report_only = match &finding {
        SoughtFinding::Found { at } => {
            let alone = members == 1
                && crate::files::companions(Path::new(at)).is_empty()
                && crate::files::companions(Path::new(&rec.src)).is_empty();
            (!alone).then_some(ReportOnly::Companion)
        }
        _ => Some(ReportOnly::NotProven),
    };
    Ok(Sought {
        src: rec.src.clone(),
        dst: rec.dst.clone(),
        scope: trees.iter().map(|t| t.display().to_string()).collect(),
        finding,
        report_only,
    })
}

/// One search, in words, as the preview and the refusal show it.
pub(crate) fn shown(s: &Sought) -> String {
    let base = match &s.finding {
        SoughtFinding::Found { at } => pc_core::tf!(
            "{0} — записанное место {1} никуда не ведёт; найден по доказательству: {2}",
            "{0} — its recorded place {1} leads nowhere; found by its evidence at {2}",
            s.src,
            s.dst,
            at
        ),
        SoughtFinding::NotFound => pc_core::tf!(
            "{0} — записанное место {1} никуда не ведёт, и в папках карантина ({2}) по \
             доказательству он не найден",
            "{0} — its recorded place {1} leads nowhere, and it was not found by its evidence \
             in the quarantine folders ({2})",
            s.src,
            s.dst,
            if s.scope.is_empty() {
                "-".to_string()
            } else {
                s.scope.join(", ")
            }
        ),
        SoughtFinding::Ambiguous { at } => pc_core::tf!(
            "{0} — записанное место {1} никуда не ведёт; к нему ведёт больше одного имени: {2}",
            "{0} — its recorded place {1} leads nowhere; more than one name leads to it: {2}",
            s.src,
            s.dst,
            at.join(", ")
        ),
        SoughtFinding::Changed { at, why } => pc_core::tf!(
            "{0} — записанное место {1} никуда не ведёт; в {2} лежит файл с его устройством и \
             inode, но доказательство не сходится ({3})",
            "{0} — its recorded place {1} leads nowhere; at {2} is a file with its device and \
             inode, but the evidence does not hold ({3})",
            s.src,
            s.dst,
            at,
            why
        ),
        SoughtFinding::NoEvidence => pc_core::tf!(
            "{0} — записанное место {1} никуда не ведёт, а запись не хранит доказательства: \
             по имени его не ищут",
            "{0} — its recorded place {1} leads nowhere, and the entry records no evidence: it \
             is not looked for by name",
            s.src,
            s.dst
        ),
    };
    match s.report_only {
        None => base,
        Some(ReportOnly::Companion) => pc_core::tf!(
            "{0}; у кадра есть спутник — сам инструмент возвращает только кадр без спутников, \
             верните вручную",
            "{0}; the frame has a companion — the tool itself brings back only a frame alone, \
             return it by hand",
            base
        ),
        Some(ReportOnly::NotProven | ReportOnly::NoEvidence) => {
            pc_core::tf!("{0}; ничего не переносится", "{0}; nothing is moved", base)
        }
    }
}

/// A recovery that searched and moved nothing: what it found, and the
/// place the preview it was bound to showed, if it was.
#[derive(Debug, Clone)]
pub struct Unrecovered {
    pub sought: Vec<Sought>,
    pub previewed: Option<String>,
    pub text: String,
}

impl std::fmt::Display for Unrecovered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Unrecovered {}

/// The searched-and-unmoved outcome inside `e`, if that is what it is.
pub fn unrecovered(e: &anyhow::Error) -> Option<&Unrecovered> {
    e.downcast_ref::<Unrecovered>()
}

/// The place found and the place previewed are one.
pub(crate) fn same_place(found: &str, previewed: &str) -> bool {
    found == previewed || spelled(Path::new(previewed)) == Path::new(found)
}
