//! Quarantine, undo and purge.
//!
//! Nothing is ever deleted by `quarantine`. A bundle is moved to
//! `<mount>/.photo-cleanup-quarantine/<original path relative to mount>`,
//! which is on the same filesystem and therefore a `rename(2)`: instant, and
//! instantly reversible. Space comes back only at `purge`.

pub mod files;
pub mod organize;

pub use files::{apply, companions, same_picture, ApplyReport, FileOutcome};
pub use organize::{organize, undo_run, OrganizeReport};

use anyhow::{bail, Context, Result};
use pc_core::{fmt_bytes, Disk};
use pc_db::{Bundle, BundleState, Db, JournalStatus};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Moved,
    Skipped,
}

#[derive(Debug, Default)]
pub struct Totals {
    /// Things moved: a bundle of previews, or a photograph.
    pub bundles: u64,
    pub files: u64,
    pub bytes: u64,
    pub skipped: Vec<String>,
}

impl Totals {
    pub fn summary(&self) -> String {
        format!(
            "{}, {}, {}",
            pc_core::count_ru(self.bundles as i64, "объект", "объекта", "объектов"),
            pc_core::count_ru(self.files as i64, "файл", "файла", "файлов"),
            fmt_bytes(self.bytes)
        )
    }
}

fn disk_of(b: &Bundle) -> Disk {
    Disk {
        dev: b.dev as u64,
        mount: PathBuf::from(&b.mount),
        label: b.disk.clone(),
    }
}

/// Device of `path`, or of its nearest existing ancestor when it does not
/// exist yet.
/// Where a bundle goes when quarantined.
///
/// The default sits at the root of the bundle's own filesystem, which makes
/// the move a `rename(2)`. `override_root` exists for the case where that root
/// is not writable — a single-filesystem host, for instance — and is rejected
/// unless it lives on the same device, because a cross-device "move" would
/// silently become a copy of the whole bundle.
pub fn quarantine_dest(b: &Bundle, override_root: Option<&Path>) -> Result<PathBuf> {
    let disk = disk_of(b);
    let src = PathBuf::from(&b.path);
    let rel = disk.relative(&src);

    match override_root {
        None => Ok(disk.quarantine_root().join(rel)),
        Some(root) => {
            let dev = pc_core::dev_of_nearest_existing(root)?;
            if dev != b.dev as u64 {
                bail!(
                    "карантин {} находится на другой файловой системе, чем {} — \
                     перенос превратился бы в полное копирование. Укажите путь на том же диске.",
                    root.display(),
                    b.path
                );
            }
            Ok(root.join(&b.disk).join(rel))
        }
    }
}

/// Where an indexed file goes when quarantined, mirroring its path under the
/// quarantine root on its own filesystem.
pub fn quarantine_dest_for(
    path: &str,
    _file_id: i64,
    _db: &Db,
    override_root: Option<&Path>,
) -> Result<PathBuf> {
    let src = PathBuf::from(path);
    let mut map = pc_core::DiskMap::new();
    let disk = map.resolve(&src)?;
    let rel = disk.relative(&src);
    match override_root {
        None => Ok(disk.quarantine_root().join(rel)),
        Some(root) => {
            let dev = pc_core::dev_of_nearest_existing(root)?;
            if dev != disk.dev {
                bail!(
                    "карантин {} на другой файловой системе, чем {path} — \
                     перенос превратился бы в копирование",
                    root.display()
                );
            }
            Ok(root.join(&disk.label).join(rel))
        }
    }
}

/// Re-check that what is on disk still matches what was scanned.
///
/// A bundle that changed since the scan is skipped rather than moved: the
/// user may have reopened the catalog and Lightroom may be writing into it.
fn unchanged(b: &Bundle) -> Result<bool> {
    let path = Path::new(&b.path);
    if !path.exists() {
        return Ok(false);
    }
    if b.is_dir {
        let (count, size, newest) = dir_stats(path);
        Ok(count as i64 == b.file_count && size as i64 == b.size && newest == b.newest_mtime)
    } else {
        let md = fs::metadata(path)?;
        Ok(md.len() as i64 == b.size && pc_core::time::mtime_unix(&md) == b.newest_mtime)
    }
}

fn dir_stats(root: &Path) -> (u64, u64, i64) {
    let mut count = 0u64;
    let mut size = 0u64;
    let mut newest = 0i64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            newest = newest.max(pc_core::time::mtime_unix(&md));
            if ft.is_dir() {
                stack.push(e.path());
            } else if ft.is_file() {
                count += 1;
                size += md.len();
            }
        }
    }
    if let Ok(md) = fs::metadata(root) {
        newest = newest.max(pc_core::time::mtime_unix(&md));
    }
    (count, size, newest)
}

pub(crate) fn rename_with_parents(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "не создать каталог карантина {} — если файловая система смонтирована \
                 в корень или недоступна на запись, задайте --quarantine <путь на том же диске>",
                parent.display()
            )
        })?;
    }
    if dst.exists() {
        bail!("цель уже существует: {}", dst.display());
    }
    fs::rename(src, dst).with_context(|| {
        format!(
            "не переместить {} -> {} (перенос обязан быть в пределах одного диска)",
            src.display(),
            dst.display()
        )
    })
}

/// Move one bundle into quarantine, journalling before touching the filesystem.
pub fn quarantine(
    db: &Db,
    run_id: i64,
    b: &Bundle,
    override_root: Option<&Path>,
) -> Result<Outcome> {
    if !b.regenerable {
        bail!(
            "{} относится к виду «{}», удаление запрещено",
            b.path,
            b.kind.label()
        );
    }
    if let Some(code) = &b.blocked_code {
        bail!("{} заблокирован: {code}", b.path);
    }
    if b.state != BundleState::Present {
        return Ok(Outcome::Skipped);
    }
    if !unchanged(b)? {
        return Ok(Outcome::Skipped);
    }

    let dst = quarantine_dest(b, override_root)?;
    let dst_str = dst.to_string_lossy().into_owned();
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "quarantine",
        target_id: Some(b.id),
        src: &b.path,
        dst: Some(&dst_str),
        size: b.size,
        file_count: b.file_count,
    })?;

    match rename_with_parents(Path::new(&b.path), &dst) {
        Ok(()) => {
            db.journal_finish(jid, JournalStatus::Done, None)?;
            db.set_bundle_state(b.id, BundleState::Quarantined)?;
            Ok(Outcome::Moved)
        }
        Err(e) => {
            db.journal_finish(jid, JournalStatus::Failed, Some(&e.to_string()))?;
            Err(e)
        }
    }
}

pub fn quarantine_many(
    db: &Db,
    run_id: i64,
    bundles: &[Bundle],
    override_root: Option<&Path>,
) -> Result<Totals> {
    let mut t = Totals::default();
    for b in bundles {
        match quarantine(db, run_id, b, override_root) {
            Ok(Outcome::Moved) => {
                t.bundles += 1;
                t.files += b.file_count as u64;
                t.bytes += b.size as u64;
            }
            Ok(Outcome::Skipped) => t
                .skipped
                .push(format!("{} — изменился с момента сканирования", b.path)),
            Err(e) => t.skipped.push(format!("{} — {e}", b.path)),
        }
    }
    Ok(t)
}

/// Move a quarantined bundle back where it came from.
pub fn undo(db: &Db, journal_id: i64) -> Result<()> {
    let entry = db
        .journal_entry(journal_id)?
        .with_context(|| format!("нет записи журнала {journal_id}"))?;
    if entry.status != JournalStatus::Done {
        bail!(
            "запись {journal_id} в состоянии «{}», откат невозможен",
            entry.status.as_str()
        );
    }
    let dst = entry.dst.clone().context("в записи нет пути назначения")?;
    let dst_path = PathBuf::from(&dst);
    let src_path = PathBuf::from(&entry.src);
    let sidecars = files::companions(&dst_path);
    rename_with_parents(&dst_path, &src_path)?;

    let name_of = |p: &Path| -> String {
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string()
    };
    // Sidecars that travelled with the photograph come home with it. A
    // reorganisation may have renamed the file out of a name collision, in
    // which case they carry the new stem and have to be carried back.
    let new_stem = organize::stem_of(&name_of(&dst_path)).to_string();
    let old_stem = organize::stem_of(&name_of(&src_path)).to_string();
    for side in sidecars {
        if let Some(name) = side.file_name().and_then(|s| s.to_str()) {
            let back = src_path.with_file_name(organize::sidecar_name(name, &new_stem, &old_stem));
            let _ = rename_with_parents(&side, &back);
        }
    }
    // The directories the file came out of are ours to remove only while
    // they are empty; `remove_dir` declines to take away anything else, and
    // the archive's own roots are never touched.
    if let Some(parent) = dst_path.parent() {
        let roots = db.all_run_roots()?.into_iter().map(PathBuf::from).collect();
        organize::prune_empty(
            &[parent.to_path_buf()].into_iter().collect(),
            &roots,
            organize::UNDO_LEVELS,
        );
    }

    db.journal_mark_undone(journal_id)?;
    match (entry.op.as_str(), entry.target_id) {
        ("quarantine", Some(bid)) => db.set_bundle_state(bid, BundleState::Present)?,
        ("quarantine-file", Some(fid)) => db.set_file_state(fid, "present")?,
        ("organize", Some(fid)) => db.set_file_path(fid, &entry.src, &name_of(&src_path))?,
        _ => {}
    }
    Ok(())
}

/// Permanently remove quarantined data older than `older_than_secs`.
///
/// This is the only destructive operation in the tool.
pub fn purge(db: &Db, older_than_secs: i64) -> Result<Totals> {
    let cutoff = pc_core::time::now_unix() - older_than_secs;
    let entries = db.journal_quarantined(Some(cutoff))?;
    let mut t = Totals::default();
    for e in entries {
        let Some(dst) = e.dst.clone() else { continue };
        let path = Path::new(&dst);
        let res = if path.is_dir() {
            fs::remove_dir_all(path)
        } else if path.exists() {
            fs::remove_file(path)
        } else {
            Ok(()) // already gone
        };
        match res {
            Ok(()) => {
                db.journal_mark_purged(e.id)?;
                // Only a bundle has a state to move; a photograph's row is
                // identified by the journal entry alone.
                match (e.op.as_str(), e.target_id) {
                    ("quarantine", Some(bid)) => db.set_bundle_state(bid, BundleState::Purged)?,
                    ("quarantine-file", Some(fid)) => db.set_file_state(fid, "purged")?,
                    _ => {}
                }
                t.bundles += 1;
                t.files += e.file_count as u64;
                t.bytes += e.size as u64;
            }
            Err(err) => t.skipped.push(format!("{dst} — {err}")),
        }
    }
    Ok(t)
}

/// What is currently sitting in quarantine, not yet purged.
pub fn quarantined_totals(db: &Db) -> Result<Totals> {
    let mut t = Totals::default();
    for e in db.journal_quarantined(None)? {
        t.bundles += 1;
        t.files += e.file_count as u64;
        t.bytes += e.size as u64;
    }
    Ok(t)
}
