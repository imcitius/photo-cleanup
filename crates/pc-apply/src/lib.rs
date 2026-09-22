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

/// Where a file goes inside a gathered quarantine, and the note that says so.
///
/// Under the same hidden name the beside-quarantine uses, for two reasons.
/// The walk knows that name and steps over it, so a later scan cannot index
/// quarantined files as photographs of the archive — with a plain folder name
/// it did exactly that. And whatever reads a quarantine afterwards finds it
/// by the same mark wherever it sits.
///
/// Beside the data goes a note of what each disk label stood for. The label
/// alone — `disk3` — means nothing once the database is gone, and that is
/// precisely when this has to be readable.
fn gathered(root: &Path, label: &str, mount: &Path, rel: &Path) -> Result<PathBuf> {
    let home = root.join(pc_core::QUARANTINE_DIR);
    pc_core::quarantine_layout::note(&home, label, mount).with_context(|| {
        pc_core::tf!(
            "не записать раскладку карантина в {0}",
            "cannot record the quarantine layout in {0}",
            home.display()
        )
    })?;
    Ok(home.join(label).join(rel))
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
            gathered(root, &b.disk, &disk.mount, rel)
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
            gathered(root, &disk.label, &disk.mount, rel)
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

/// The catalogue's own answer, asked at the moment of the move.
///
/// A scan writes down what it found — and by the time a plan is carried out,
/// Lightroom may have been opened, or the originals a smart preview stands in
/// for may have gone. The saved verdict cannot know that; only the disk can.
/// This is the check every path takes, HTTP and command line alike, so that
/// "the catalogue is open" is a property of the operation and not of one
/// interface to it.
pub fn lightroom_gate(b: &Bundle) -> Result<()> {
    let Some(owner) = &b.owner_ref else {
        return Ok(());
    };
    if Path::new(&format!("{owner}.lock")).exists() {
        bail!(
            "{}",
            pc_core::tf!(
                "Каталог Lightroom открыт: {0}",
                "The Lightroom catalogue is open: {0}",
                owner
            )
        );
    }
    // A smart preview is the only copy of a frame whose original is not
    // there. Standing in for nothing, it stops being derived data.
    if b.kind == pc_core::DerivedKind::LrSmartPreviews && Path::new(owner).exists() {
        let check = pc_lightroom::check_originals(Path::new(owner))?;
        if !check.all_present() {
            bail!(
                "{}",
                pc_core::tf!(
                    "Отсутствуют оригиналы: {0} из {1} — {2}",
                    "Originals are missing: {0} of {1} — {2}",
                    check.missing,
                    check.total,
                    owner
                )
            );
        }
    }
    Ok(())
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
    lightroom_gate(b)?;

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

/// Carry a file the journal never claimed back out of quarantine.
///
/// Left by a database that is no longer here: this one has no row saying how
/// it got there, so it writes one now, and the move can be walked back like
/// any other.
pub fn adopt_orphan(db: &Db, run_id: i64, src: &str, dst: &str) -> Result<()> {
    if Path::new(dst).exists() {
        bail!(
            "{}",
            pc_core::tf!(
                "на месте уже лежит файл: {0}",
                "a file is already back in place: {0}",
                dst
            )
        );
    }
    let size = fs::metadata(src).map(|m| m.len()).unwrap_or(0) as i64;
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "adopt",
        target_id: None,
        src,
        dst: Some(dst),
        size,
        file_count: 1,
        manifest: &[pc_db::Moved {
            src: src.to_string(),
            dst: dst.to_string(),
        }],
    })?;
    match rename_with_parents(Path::new(src), Path::new(dst)) {
        Ok(()) => {
            db.journal_finish(jid, JournalStatus::Done, None)?;
            Ok(())
        }
        Err(e) => {
            db.journal_finish(jid, JournalStatus::Failed, Some(&e.to_string()))?;
            Err(e)
        }
    }
}

/// Delete a file the journal never claimed. There is nothing to move it back
/// from, so the row is written first and the bytes go second.
pub fn abandon_orphan(
    db: &Db,
    run_id: i64,
    path: &str,
    control: &pc_core::work::Control,
) -> Result<()> {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0) as i64;
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "abandon",
        target_id: None,
        src: path,
        dst: None,
        size,
        file_count: 1,
        manifest: &[],
    })?;
    match remove_controlled(Path::new(path), control) {
        Ok(()) => {
            db.journal_mark_purged(jid)?;
            Ok(())
        }
        Err(e) => {
            db.journal_finish(jid, JournalStatus::Failed, Some(&format!("{e:#}")))?;
            Err(e)
        }
    }
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
        // Unless it already has — an undo that failed halfway can be asked
        // for again, and the retry must carry back what is still in
        // quarantine without touching what is already home.
        if dst_path.exists() || !src_path.exists() {
            rename_with_parents(&dst_path, &src_path)?;
        }
        for m in entry.manifest.iter().filter(|m| m.src != entry.src) {
            let (from, to) = (Path::new(&m.dst), Path::new(&m.src));
            if !from.exists() {
                if to.exists() {
                    // Came back on an earlier attempt. Nothing to report and
                    // nothing to do.
                    continue;
                }
                failed.push(pc_core::tf!(
                    "{0} — файла нет в карантине",
                    "{0} — not in quarantine any more",
                    m.dst
                ));
                continue;
            }
            if let Err(e) = rename_with_parents(from, to) {
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

    // What did come back, the index should say is back: the photograph is in
    // the archive whether or not its sidecar managed to follow.
    //
    // Which row this concerns is decided by path, not by the id the entry was
    // written with. After a reset those ids belong to other files, and an
    // entry from an older database would otherwise reach into the new index
    // and change a stranger.
    match entry.op.as_str() {
        "quarantine" => {
            if let Some(id) = db.bundle_id_at(&entry.src)? {
                db.set_bundle_state(id, BundleState::Present)?;
            }
        }
        "quarantine-file" => {
            if let Some(id) = db.file_id_at(&entry.src)? {
                db.set_file_state(id, "present")?;
            }
        }
        // The reorganisation moved the file, so the index knows it by where
        // it was moved to.
        "organize" => {
            if let Some(id) = db.file_id_at(&dst)? {
                db.set_file_path(id, &entry.src, &name_of(&src_path))?;
            }
        }
        _ => {}
    }
    if !failed.is_empty() {
        // Half an undo is not an undo. Marking the entry `undone` would close
        // the only door back to what stayed behind — the entry would stop
        // being offered, and a sidecar holding a photograph's edits would sit
        // in quarantine with nothing left pointing at it. It stays `done`,
        // with the reason written down, and asking again carries on from
        // where this stopped.
        let why = pc_core::tf!(
            "откат: не вернулось {0}",
            "undo: did not come back — {0}",
            failed.join("; ")
        );
        db.journal_finish(journal_id, JournalStatus::Done, Some(&why))?;
        bail!("{why}");
    }
    db.journal_mark_undone(journal_id)?;
    Ok(())
}

/// What an interrupted operation actually did, item by item.
///
/// The journal is written before the disk is touched and finished afterwards,
/// so a killed process leaves a `pending` row: the list of what it meant to
/// move, and no word on how far it got. That row is deliberately not offered
/// as a whole reversible operation — it is not one — and until now that was
/// the end of it. "Check the journal" is advice, not an operation, and the
/// files stayed where the interruption left them.
///
/// This reads the manifest against the disk and says, for every file, which
/// of four states it is in. Three of them are answers; one of them is a
/// question only a person can settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// At its source. Either it never moved, or it has already been brought
    /// back — nothing to do either way.
    Home,
    /// In quarantine and nowhere else: this one moved, and can come back.
    Moved,
    /// At both paths. The tool will not choose between two files, and it will
    /// not overwrite either.
    Both,
    /// At neither. Something outside this tool has been here.
    Gone,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub src: String,
    pub dst: String,
    pub standing: Standing,
}

/// Read a `pending` entry against the disk.
pub fn reconcile(db: &Db, journal_id: i64) -> Result<Vec<Item>> {
    let entry = db.journal_entry(journal_id)?.with_context(|| {
        pc_core::tf!("нет записи журнала {0}", "no journal entry {0}", journal_id)
    })?;
    if entry.status != JournalStatus::Pending {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0} в состоянии «{1}»: сверять нечего",
                "entry {0} is “{1}”: there is nothing to reconcile",
                journal_id,
                entry.status.as_str()
            )
        );
    }
    // A purge is not reversible and never was: the bytes it removed are gone,
    // and an interrupted one leaves nothing to carry back. Saying what is
    // missing is all this can honestly do, so it does not pretend otherwise.
    if entry.op.contains("purge") {
        bail!(
            "{}",
            pc_core::tr!(
                "прерванное окончательное удаление не восстанавливается: проверьте свою резервную копию",
                "an interrupted permanent deletion cannot be undone: check your own backup"
            )
        );
    }
    let pairs: Vec<(String, String)> = if entry.manifest.is_empty() {
        // Written before the journal held a list. One pair is all it knows.
        let dst = entry.dst.clone().context(pc_core::tr!(
            "в записи нет пути назначения",
            "the entry has no destination path"
        ))?;
        vec![(entry.src.clone(), dst)]
    } else {
        entry
            .manifest
            .iter()
            .map(|m| (m.src.clone(), m.dst.clone()))
            .collect()
    };
    Ok(pairs
        .into_iter()
        .map(|(src, dst)| {
            let standing = match (Path::new(&src).exists(), Path::new(&dst).exists()) {
                (true, true) => Standing::Both,
                (true, false) => Standing::Home,
                (false, true) => Standing::Moved,
                (false, false) => Standing::Gone,
            };
            Item { src, dst, standing }
        })
        .collect())
}

/// Bring back what an interrupted operation moved, and close its entry.
///
/// Only the unambiguous ones. A file sitting at both paths is two files, and
/// choosing between them is not this tool's decision; a file at neither is
/// not this tool's doing. Either of those leaves the entry `pending`, which
/// is what it is, with the reason written down.
pub fn reconcile_undo(db: &Db, journal_id: i64) -> Result<Vec<Item>> {
    let items = reconcile(db, journal_id)?;
    let unclear: Vec<&Item> = items
        .iter()
        .filter(|i| matches!(i.standing, Standing::Both | Standing::Gone))
        .collect();
    if !unclear.is_empty() {
        let why = unclear
            .iter()
            .map(|i| match i.standing {
                Standing::Both => pc_core::tf!(
                    "{0} — файл есть и на исходном месте, и в карантине",
                    "{0} — the file is at its source and in quarantine",
                    i.src
                ),
                _ => pc_core::tf!(
                    "{0} — файла нет ни там, ни там",
                    "{0} — the file is at neither path",
                    i.src
                ),
            })
            .collect::<Vec<_>>()
            .join("; ");
        db.journal_finish(journal_id, JournalStatus::Pending, Some(&why))?;
        bail!("{why}");
    }
    for item in items.iter().filter(|i| i.standing == Standing::Moved) {
        rename_with_parents(Path::new(&item.dst), Path::new(&item.src))?;
    }
    let entry = db.journal_entry(journal_id)?.expect("read a moment ago");
    if entry.op == "quarantine-file" {
        if let Some(id) = db.file_id_at(&entry.src)? {
            db.set_file_state(id, "present")?;
        }
    } else if entry.op == "quarantine" {
        if let Some(id) = db.bundle_id_at(&entry.src)? {
            db.set_bundle_state(id, BundleState::Present)?;
        }
    }
    // Nothing of this operation is left in quarantine, which is what `undone`
    // says. It never finished, and the note keeps that fact.
    db.journal_finish(
        journal_id,
        JournalStatus::Done,
        Some(pc_core::tr!(
            "прерванная операция сверена по манифесту и отменена",
            "an interrupted operation was reconciled against its manifest and undone"
        )),
    )?;
    db.journal_mark_undone(journal_id)?;
    Ok(items)
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
    // By path, for the same reason as in `undo`: the number in the entry may
    // now belong to a file that is still in the archive, and marking that one
    // purged would take it out of every view while its bytes sit untouched.
    match e.op.as_str() {
        "quarantine" => {
            if let Some(id) = db.bundle_id_at(&e.src)? {
                db.set_bundle_state(id, BundleState::Purged)?;
            }
        }
        "quarantine-file" => {
            if let Some(id) = db.file_id_at(&e.src)? {
                db.set_file_state(id, "purged")?;
            }
        }
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

#[cfg(test)]
mod lightroom_tests {
    use super::*;
    use pc_db::{model::NewBundle, Db};

    /// A bundle of previews recorded by a scan, exactly as the walk writes it.
    fn scanned(db: &Db, run: i64, dir: &Path, owner: &Path) -> pc_db::Bundle {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("cache"), b"cached").unwrap();
        db.upsert_bundle(
            &NewBundle {
                path: dir.display().to_string(),
                is_dir: true,
                disk: "root".into(),
                dev: 0,
                mount: dir.parent().unwrap().display().to_string(),
                kind: pc_core::DerivedKind::LrPreviews,
                owner_ref: Some(owner.display().to_string()),
                file_count: 1,
                size: 6,
                newest_mtime: pc_core::time::mtime_unix(&fs::metadata(dir).unwrap()),
            },
            run,
        )
        .unwrap();
        db.list_bundles(&Default::default())
            .unwrap()
            .into_iter()
            .find(|b| b.path == dir.display().to_string())
            .unwrap()
    }

    #[test]
    fn a_catalogue_opened_after_the_scan_stops_the_move_in_the_core() {
        // The scan wrote down that nothing blocked these previews. Lightroom
        // was opened afterwards, and rebuilding previews it is holding is not
        // the tool's decision to make. The web preview used to be the only
        // place that looked again — the command line went straight past it.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        let previews = root.join("Library Previews.lrdata");
        let owner = root.join("Library.lrcat");
        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        let b = scanned(&db, run, &previews, &owner);
        assert!(
            b.removable(),
            "сценарий не тот: скан уже что-то заблокировал"
        );

        fs::write(root.join("Library.lrcat.lock"), b"open").unwrap();
        let refused = quarantine(&db, run, &b, None).unwrap_err().to_string();
        assert!(refused.contains("Library.lrcat"), "{refused}");
        assert!(previews.exists(), "превью уехали при открытом каталоге");

        // Closed again, and the same call goes through.
        fs::remove_file(root.join("Library.lrcat.lock")).unwrap();
        assert_eq!(quarantine(&db, run, &b, None).unwrap(), Outcome::Moved);
        assert!(!previews.exists());
    }
}

#[cfg(test)]
mod undo_tests {
    use super::*;
    use pc_db::{Db, JournalStatus, Moved, NewJournalEntry};

    /// One quarantined photograph and its sidecar, as the journal records it.
    fn quarantined(db: &Db, run: i64, dir: &Path) -> (i64, PathBuf, PathBuf) {
        let (src, dst) = (dir.join("archive/a.png"), dir.join("quarantine/a.png"));
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::write(&dst, b"photo").unwrap();
        fs::write(dst.with_extension("xmp"), b"the edits that went with it").unwrap();
        let (s, d) = (src.display().to_string(), dst.display().to_string());
        let id = db
            .journal_begin(&NewJournalEntry {
                run_id: run,
                op: "quarantine-file",
                target_id: None,
                src: &s,
                dst: Some(&d),
                size: 32,
                file_count: 2,
                manifest: &[
                    Moved {
                        src: s.clone(),
                        dst: d.clone(),
                    },
                    Moved {
                        src: src.with_extension("xmp").display().to_string(),
                        dst: dst.with_extension("xmp").display().to_string(),
                    },
                ],
            })
            .unwrap();
        db.journal_finish(id, JournalStatus::Done, None).unwrap();
        (id, src, dst)
    }

    #[test]
    fn an_undo_that_could_not_finish_says_so_and_can_be_asked_again() {
        // Half an undo is not an undo. The photograph came home and its
        // sidecar could not follow, because something was already sitting
        // where it belonged — and the entry was marked undone anyway. That
        // closed the only door back: the entry stopped being offered, and a
        // file holding a photograph's edits sat in quarantine with nothing
        // left pointing at it.
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        let run = db.start_run(&[], "test").unwrap();
        let (id, src, dst) = quarantined(&db, run, tmp.path());
        let occupied = src.with_extension("xmp");
        fs::write(&occupied, b"newer edits").unwrap();

        let refused = undo(&db, id).unwrap_err();

        assert!(src.exists(), "снимок не вернулся");
        assert!(dst.with_extension("xmp").exists(), "чужой файл затёрт");
        assert_eq!(fs::read(&occupied).unwrap(), b"newer edits");
        assert!(format!("{refused:#}").contains("xmp"), "{refused:#}");
        let entry = db.journal_entry(id).unwrap().unwrap();
        assert_eq!(
            entry.status,
            JournalStatus::Done,
            "частичный откат объявлен завершённым"
        );

        // And asking again carries on rather than starting over: the
        // photograph is already home and must not be moved a second time.
        fs::remove_file(&occupied).unwrap();
        undo(&db, id).unwrap();
        assert!(src.exists());
        assert!(occupied.exists(), "спутник не вернулся со второй попытки");
        assert_eq!(
            db.journal_entry(id).unwrap().unwrap().status,
            JournalStatus::Undone
        );
    }
}
