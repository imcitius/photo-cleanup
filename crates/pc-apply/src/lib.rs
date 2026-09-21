//! Quarantine, undo and purge.
//!
//! Nothing is ever deleted by `quarantine`. A file is moved into a hidden
//! `.photo-cleanup-quarantine` folder *in its own directory*, which is on the
//! same filesystem and therefore a `rename(2)`: instant, and instantly
//! reversible. Space comes back only at `purge`.
//!
//! It used to go to the root of the file's filesystem instead. That works on a
//! NAS, where the archive sits on `/mnt/diskN` and the root of that mount is
//! writable. On an ordinary machine the mount root is `/`, which is not — so
//! every move failed with "не создать каталог карантина /.photo-cleanup-…".
//! Beside the file there is no such question: whatever directory a photograph
//! can be removed from, it can also be written to.

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
            pc_core::count(
                self.bundles as i64,
                ["объект", "объекта", "объектов"],
                ["object", "objects"]
            ),
            pc_core::count(
                self.files as i64,
                ["файл", "файла", "файлов"],
                ["file", "files"]
            ),
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
/// The hidden folder beside `src` that holds what was moved out of its
/// directory, and the path `src` takes inside it.
fn beside(src: &Path) -> Result<PathBuf> {
    let parent = src
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .with_context(|| {
            pc_core::tf!(
                "{0} — у пути нет родительского каталога",
                "{0} — the path has no parent directory",
                src.display()
            )
        })?;
    let name = src.file_name().with_context(|| {
        pc_core::tf!(
            "{0} — у пути нет имени файла",
            "{0} — the path has no file name",
            src.display()
        )
    })?;
    Ok(parent.join(pc_core::QUARANTINE_DIR).join(name))
}

/// Where a bundle goes when quarantined.
///
/// Beside itself by default, so the move is a rename and the directory is one
/// that already takes writes. `override_root` gathers everything in one place
/// instead, and is rejected unless it lives on the same device, because a
/// cross-device "move" would silently become a copy of the whole bundle.
pub fn quarantine_dest(b: &Bundle, override_root: Option<&Path>) -> Result<PathBuf> {
    let disk = disk_of(b);
    let src = PathBuf::from(&b.path);
    let rel = disk.relative(&src);

    match override_root {
        None => beside(&src),
        Some(root) => {
            let dev = pc_core::dev_of_nearest_existing(root)?;
            if dev != b.dev as u64 {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "карантин {0} находится на другой файловой системе, чем {1} — перенос превратился бы в полное копирование. Укажите путь на том же диске.",
                        "quarantine {0} is on a different filesystem from {1} — the move would become a full copy. Give a path on the same disk.",
                        root.display(),
                        b.path
                    )
                );
            }
            Ok(root.join(&b.disk).join(rel))
        }
    }
}

/// Where an indexed file goes when quarantined.
///
/// Beside itself by default; under `override_root`, mirroring its path from
/// the mount point so two files of the same name do not collide.
pub fn quarantine_dest_for(
    path: &str,
    _file_id: i64,
    _db: &Db,
    override_root: Option<&Path>,
) -> Result<PathBuf> {
    let src = PathBuf::from(path);
    if override_root.is_none() {
        return beside(&src);
    }
    let mut map = pc_core::DiskMap::new();
    let disk = map.resolve(&src)?;
    let rel = disk.relative(&src);
    match override_root {
        None => unreachable!("handled above"),
        Some(root) => {
            let dev = pc_core::dev_of_nearest_existing(root)?;
            if dev != disk.dev {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "карантин {0} на другой файловой системе, чем {1} — перенос превратился бы в копирование",
                        "quarantine {0} is on a different filesystem from {1} — the move would become a copy",
                        root.display(),
                        path
                    )
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
        bail!(
            "{}",
            pc_core::tf!(
                "цель уже существует: {0}",
                "the destination already exists: {0}",
                dst.display()
            )
        );
    }
    fs::rename(src, dst).with_context(|| {
        pc_core::tf!(
            "не переместить {0} -> {1} (перенос обязан быть в пределах одного диска)",
            "cannot move {0} -> {1} (a move has to stay within one disk)",
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
            "{}",
            pc_core::tf!(
                "{0} относится к виду «{1}», удаление запрещено",
                "{0} is of kind “{1}”, which is never removed",
                b.path,
                b.kind.label()
            )
        );
    }
    if let Some(code) = &b.blocked_code {
        bail!(
            "{}",
            pc_core::tf!("{0} заблокирован: {1}", "{0} is blocked: {1}", b.path, code)
        );
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
        // A bundle moves as one directory: its own path says everything.
        manifest: &[],
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
            Ok(Outcome::Skipped) => t.skipped.push(pc_core::tf!(
                "{0} — изменился с момента сканирования",
                "{0} — changed since the scan",
                b.path
            )),
            Err(e) => t.skipped.push(format!("{} — {e}", b.path)),
        }
    }
    Ok(t)
}

/// Move a quarantined bundle back where it came from.
/// Move the rest of an operation's files and say which of them made it.
///
/// A sidecar that refuses to move is a fact worth keeping: it stays out of
/// the manifest, so an undo is not surprised by a file that never left, and
/// the journal note names it.
pub(crate) fn carry(rest: &[pc_db::Moved]) -> (Vec<pc_db::Moved>, Vec<(String, String)>) {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for m in rest {
        match rename_with_parents(Path::new(&m.src), Path::new(&m.dst)) {
            Ok(()) => moved.push(m.clone()),
            Err(e) => failed.push((m.src.clone(), e.to_string())),
        }
    }
    (moved, failed)
}

/// `path — why; path — why`, for a journal note.
pub(crate) fn listed(failed: &[(String, String)]) -> String {
    failed
        .iter()
        .map(|(p, why)| format!("{p} — {why}"))
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn undo(db: &Db, journal_id: i64) -> Result<()> {
    let entry = db.journal_entry(journal_id)?.with_context(|| {
        pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
    })?;
    if entry.status != JournalStatus::Done {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0} в состоянии «{1}», откат невозможен",
                "entry {0} is “{1}”; it cannot be undone",
                journal_id,
                entry.status.as_str()
            )
        );
    }
    let dst = entry.dst.clone().context(pc_core::tr!(
        "в записи нет пути назначения",
        "the entry has no destination path"
    ))?;
    let dst_path = PathBuf::from(&dst);
    let src_path = PathBuf::from(&entry.src);
    let name_of = |p: &Path| -> String {
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string()
    };

    // The operation wrote down what it moved, so the undo carries back that
    // list and nothing else. A file found by name in the quarantine folder
    // may be a stranger's — one that was already there when this one arrived.
    let mut failed: Vec<String> = Vec::new();
    if entry.manifest.is_empty() {
        // Written before the journal held a list: name matching is all there
        // is, and it is why the list exists now.
        let sidecars = files::companions(&dst_path);
        rename_with_parents(&dst_path, &src_path)?;
        let new_stem = organize::stem_of(&name_of(&dst_path)).to_string();
        let old_stem = organize::stem_of(&name_of(&src_path)).to_string();
        for side in sidecars {
            if let Some(name) = side.file_name().and_then(|s| s.to_str()) {
                let back =
                    src_path.with_file_name(organize::sidecar_name(name, &new_stem, &old_stem));
                let _ = rename_with_parents(&side, &back);
            }
        }
    } else {
        // The photograph first: if it cannot come back, nothing should move.
        rename_with_parents(&dst_path, &src_path)?;
        for m in entry.manifest.iter().filter(|m| m.src != entry.src) {
            let from = Path::new(&m.dst);
            if !from.exists() {
                failed.push(pc_core::tf!(
                    "{0} — файла нет в карантине",
                    "{0} — not in quarantine any more",
                    m.dst
                ));
                continue;
            }
            if let Err(e) = rename_with_parents(from, Path::new(&m.src)) {
                failed.push(format!("{} — {e}", m.dst));
            }
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

    if !failed.is_empty() {
        // The photograph is home; saying so quietly while a sidecar stayed
        // behind is how an archive loses its edits.
        db.journal_finish(
            journal_id,
            JournalStatus::Done,
            Some(&pc_core::tf!(
                "откат: не вернулось {0}",
                "undo: did not come back — {0}",
                failed.join("; ")
            )),
        )?;
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
        match purge_entry(db, e.id) {
            Ok(()) => {
                t.bundles += 1;
                t.files += e.file_count as u64;
                t.bytes += e.size as u64;
            }
            Err(err) => t.skipped.push(format!("{} — {err}", e.src)),
        }
    }
    Ok(t)
}

/// Purge one previously reviewed quarantine entry.
pub fn purge_entry(db: &Db, id: i64) -> Result<()> {
    purge_entry_controlled(db, id, &pc_core::work::Control::default())
}
/// A partially purged entry is left pending: it must never be offered as
/// intact, undoable quarantine after a cancellation or server restart.
pub fn purge_entry_controlled(db: &Db, id: i64, control: &pc_core::work::Control) -> Result<()> {
    let e = db.journal_entry(id)?.context(pc_core::tr!(
        "нет записи карантина",
        "no such quarantine entry"
    ))?;
    if e.status != JournalStatus::Done || !matches!(e.op.as_str(), "quarantine" | "quarantine-file")
    {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0} не находится в карантине",
                "entry {0} is not in quarantine",
                id
            )
        );
    }
    let dst = e.dst.as_deref().context(pc_core::tr!(
        "в записи нет пути назначения",
        "the entry has no destination path"
    ))?;
    let path = Path::new(dst);
    control.check()?;
    db.journal_finish(
        id,
        JournalStatus::Pending,
        Some("Окончательное удаление начато; при прерывании часть файлов уже может отсутствовать"),
    )?;
    // Exactly what this operation moved here, when it wrote it down; for an
    // older row, whatever carries the same name beside it.
    let rest: Vec<PathBuf> = if e.manifest.is_empty() {
        files::companions(path)
    } else {
        e.manifest
            .iter()
            .filter(|m| m.src != e.src)
            .map(|m| PathBuf::from(&m.dst))
            .collect()
    };
    remove_controlled(path, control)?;
    for side in rest {
        remove_controlled(&side, control)?;
    }
    db.journal_mark_purged(id)?;
    match (e.op.as_str(), e.target_id) {
        ("quarantine", Some(id)) => db.set_bundle_state(id, BundleState::Purged)?,
        ("quarantine-file", Some(id)) => db.set_file_state(id, "purged")?,
        _ => {}
    }
    Ok(())
}

fn remove_controlled(path: &Path, control: &pc_core::work::Control) -> Result<()> {
    control.current(&path.display().to_string())?;
    match fs::symlink_metadata(path) {
        Ok(md) if md.is_dir() => {
            for entry in fs::read_dir(path).with_context(|| {
                pc_core::tf!("не прочитать {0}", "cannot read {0}", path.display())
            })? {
                remove_controlled(&entry?.path(), control)?;
            }
            fs::remove_dir(path).with_context(|| {
                pc_core::tf!("не удалить {0}", "cannot remove {0}", path.display())
            })?;
        }
        Ok(_) => fs::remove_file(path)
            .with_context(|| pc_core::tf!("не удалить {0}", "cannot remove {0}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                pc_core::tf!("не прочитать {0}", "cannot read {0}", path.display())
            })
        }
    }
    Ok(())
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
