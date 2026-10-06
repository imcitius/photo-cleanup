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

// Test seams reach into the move boundary; they are compiled only for
// tests (el-1y8uo B5). An optimised build that asks for them is refused at
// compile time, so no release artefact can carry them.
#[cfg(all(feature = "test-seams", not(debug_assertions)))]
compile_error!(
    "the `test-seams` feature of pc-apply is for tests only and cannot be compiled into a \
     release (optimised, no debug assertions) build"
);

mod bound;
pub mod files;
mod located;
pub mod organize;
pub mod outcome;
mod purge;
pub mod recovery;
mod roots;
mod unit;

pub use files::{
    apply, companion_plan, companions, same_picture, ApplyReport, Companion, FileOutcome, Filed,
};
pub use organize::{organize, undo_run, OrganizeReport};
pub use outcome::{
    is_folder_moved, is_no_exclusive_rename, is_run_stop, stop_run, stopped_run, FolderMoved,
    Halted, NoExclusiveRename, Placed, Role, Route, Stopped, Tally, Whereabouts,
};
pub use purge::{
    is_lightroom, purge_entry_controlled, purge_keeps, purge_kept, purge_stopped, purged_before,
    KeptWhy, OrphanKept, OrphanWhy, PurgeKept, PurgeStage, PurgeStopped,
};
pub use recovery::{
    reconcile, reconcile_undo, undo, undo_offered, undo_preview, Item, Reconciled, Standing,
};
pub use roots::RunRoots;

use anyhow::{bail, Context, Result};
use pc_core::fmt_bytes;
use pc_db::{BundleState, Db, JournalStatus};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Totals {
    /// Things moved: a bundle of previews, or a photograph.
    pub bundles: u64,
    pub files: u64,
    pub bytes: u64,
    /// Refused whole, nothing of it deleted: with why.
    pub skipped: Vec<String>,
    /// Stopped after deleting (a [`PurgeStopped`]): what went is in the
    /// counts above, and the entry needs a person — the command fails.
    pub stopped: Vec<String>,
    /// Kept by purge whatever their proof — every bundle, and anything of
    /// Lightroom's ([`PurgeKept`]): nothing of them deleted, each with where
    /// it is and how big, for a person to delete by hand if sure.
    pub kept: Vec<String>,
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
/// precisely when this has to be readable. Working out the destination only
/// says which note is due; it is written by [`admit`], once the file and its
/// volume have passed their checks (el-23goa: the preview and the check
/// before a run used to write it, through whatever sat at its name).
fn gathered(root: &Path, label: &str, mount: &Path, rel: &Path) -> Target {
    let home = root.join(pc_core::QUARANTINE_DIR);
    Target {
        dst: home.join(label).join(rel),
        layout: Some(Layout {
            home,
            label: label.to_string(),
            mount: mount.to_path_buf(),
        }),
    }
}

/// A gathered quarantine's note that a move into it needs.
#[derive(Debug, Clone)]
struct Layout {
    home: PathBuf,
    label: String,
    mount: PathBuf,
}

/// Where a move goes, and what has to be written before it can.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub(crate) dst: PathBuf,
    layout: Option<Layout>,
}

impl Target {
    fn beside(src: &Path) -> Result<Self> {
        Ok(Self {
            dst: beside(src)?,
            layout: None,
        })
    }
}

/// The last word before the journal and the move: the destination's volume
/// can move without replacing, and a gathered quarantine's layout note is
/// in place. Nothing is written until the volume has said yes, and the note
/// never goes through or over anything that is not ours.
pub(crate) fn admit(t: &Target) -> Result<()> {
    check_exclusive_rename(&t.dst)?;
    let Some(l) = &t.layout else {
        return Ok(());
    };
    pc_core::quarantine_layout::note(&l.home, &l.label, &l.mount).map_err(|e| {
        // Whatever the note had to leave behind is in its words: what it
        // created and kept, and any stranger it met and did not remove.
        if pc_core::disk::lacks_exclusive_rename(&e.cause) {
            NoExclusiveRename::new(l.home.join(pc_core::QUARANTINE_LAYOUT), e.to_string()).into()
        } else {
            anyhow::Error::msg(e.to_string()).context(pc_core::tf!(
                "не записать раскладку карантина в {0}",
                "cannot record the quarantine layout in {0}",
                l.home.display()
            ))
        }
    })
}

fn file_target(path: &str, override_root: Option<&Path>) -> Result<Target> {
    let src = PathBuf::from(path);
    let Some(root) = override_root else {
        return Target::beside(&src);
    };
    let mut map = pc_core::DiskMap::new();
    let disk = map.resolve(&src)?;
    let rel = disk.relative(&src);
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
    Ok(gathered(root, &disk.label, &disk.mount, rel))
}

/// Where an indexed file goes when quarantined. Only reads.
///
/// Beside itself by default; under `override_root`, mirroring its path from
/// the mount point so two files of the same name do not collide.
pub fn quarantine_dest_for(
    path: &str,
    _file_id: i64,
    _db: &Db,
    override_root: Option<&Path>,
) -> Result<PathBuf> {
    Ok(file_target(path, override_root)?.dst)
}

pub(crate) fn quarantine_target_for(path: &str, override_root: Option<&Path>) -> Result<Target> {
    file_target(path, override_root)
}

/// The nearest existing folder of `path` (itself, if it exists).
fn nearest_existing(path: &Path) -> &Path {
    let mut at = path;
    while fs::symlink_metadata(at).is_err() {
        match at.parent() {
            Some(p) if !p.as_os_str().is_empty() => at = p,
            _ => break,
        }
    }
    at
}

/// Refuse, before anything moves, a destination whose volume says it cannot
/// rename without replacing (macOS exFAT), or will not say. Where the system
/// has no such question (Linux, Windows) this passes, and the move itself
/// answers: the refused call moves nothing, and the run stops there.
pub fn check_exclusive_rename(dst: &Path) -> Result<()> {
    // Windows: the move itself refuses to replace, but nothing else this
    // crate relies on — object identity for recovery, links that are not
    // followed, removal through a held folder — has been verified on a real
    // Windows system. Until it has, nothing is moved there (el-usdqi, D4).
    #[cfg(windows)]
    {
        let _ = dst;
        bail!(
            "{}",
            pc_core::tr!(
                "на Windows перенос, откат и сверка пока не выполняются: проверка личности файлов и безопасного удаления временных файлов на настоящей системе Windows ещё не проведена",
                "on Windows, moves, undo and recovery are not carried out yet: file identity and the safe cleanup of temporary files have not been verified on a real Windows system"
            )
        );
    }
    #[cfg(not(windows))]
    check_exclusive_rename_here(dst)
}

#[cfg(not(windows))]
fn check_exclusive_rename_here(dst: &Path) -> Result<()> {
    use pc_core::disk::ExclusiveRename;
    let at = nearest_existing(dst.parent().unwrap_or(dst));
    let refused = |reason: String| -> Result<()> {
        Err(NoExclusiveRename::new(dst.to_path_buf(), reason).into())
    };
    match pc_core::disk::exclusive_rename(at) {
        Ok(ExclusiveRename::Supported | ExclusiveRename::NoQuery) => Ok(()),
        Ok(ExclusiveRename::Absent) => refused("RENAME_EXCL".into()),
        Ok(ExclusiveRename::Unreported) => refused(
            pc_core::tr!(
                "том не сообщает о RENAME_EXCL",
                "the volume does not report RENAME_EXCL"
            )
            .into(),
        ),
        Err(e) => refused(pc_core::tf!(
            "не узнать свойства тома {0}: {1}",
            "cannot read the properties of the volume of {0}: {1}",
            at.display(),
            e
        )),
    }
}

/// [`check_exclusive_rename`] for every destination of a run, before its
/// first move. Each folder is asked once.
pub(crate) fn check_all<'a>(dsts: impl IntoIterator<Item = &'a Path>) -> Result<()> {
    let mut asked = std::collections::BTreeSet::new();
    for dst in dsts {
        let folder = dst.parent().unwrap_or(dst);
        if asked.insert(folder.to_path_buf()) {
            check_exclusive_rename(dst)?;
        }
    }
    Ok(())
}

/// Before an apply of photographs: every destination volume can move
/// without replacing. Only reads (el-23goa B1). Sidecars land beside their
/// photograph, in the same folder. A photograph that is already gone is
/// refused on its own by `quarantine_file`; any other doubt about a
/// destination refuses the run before the first move.
pub fn check_candidates(
    _db: &Db,
    candidates: &[pc_family::plan::Candidate],
    override_root: Option<&Path>,
) -> Result<()> {
    let mut dsts = Vec::new();
    for c in candidates {
        match fs::symlink_metadata(&c.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(anyhow::Error::new(e).context(pc_core::tf!(
                    "не прочитать {0}",
                    "cannot read {0}",
                    c.path
                )))
            }
            Ok(_) => {}
        }
        dsts.push(file_target(&c.path, override_root)?.dst);
    }
    check_all(dsts.iter().map(PathBuf::as_path))
}

/// Before a reorganisation: as [`check_candidates`].
pub fn check_organize(moves: &[pc_organize::Move]) -> Result<()> {
    check_all(moves.iter().map(|m| Path::new(&m.dst)))
}

/// Tests only: one bound move of one object, outside any operation — the
/// boundary every unit's member goes through ([`unit::move_unit`]).
#[cfg(test)]
pub(crate) fn rename_with_parents(
    src: &Path,
    dst: &Path,
    expect: Option<&pc_core::proof::Proof>,
) -> Result<bound::Arrived> {
    bound::rename_bound(&bound::Held::bind(src, expect)?, dst, &[])
}

/// Tests only: the moment after an operation's last rename and before its
/// journal record ([`race::Syscall::BeforeRecord`]).
#[cfg(any(test, feature = "test-seams"))]
pub(crate) fn before_record(m: &pc_db::Moved) {
    let _ = race::fire_syscall(
        race::Syscall::BeforeRecord,
        Path::new(&m.src),
        Path::new(&m.dst),
    );
}

#[cfg(not(any(test, feature = "test-seams")))]
#[inline(always)]
pub(crate) fn before_record(_: &pc_db::Moved) {}

/// Close a refused operation's row: its words, and every object it touched
/// with its proven place (el-lvtmk R5).
pub(crate) fn close_refused(
    db: &Db,
    jid: i64,
    phase: &str,
    shown: &str,
    told: &located::Told,
) -> Result<()> {
    db.journal_close(
        jid,
        JournalStatus::Failed,
        &pc_db::Event {
            text: shown,
            error: Some(shown),
            located: &told.located,
            ..pc_db::Event::new(phase, "refused")
        },
    )
}

/// What a refused rename says.
pub(crate) fn rename_error(e: std::io::Error, src: &Path, dst: &Path) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        anyhow::anyhow!(
            "{}",
            pc_core::tf!(
                "цель уже существует и не будет заменена: {0}",
                "the destination already exists and is not replaced: {0}",
                dst.display()
            )
        )
    } else if pc_core::disk::lacks_exclusive_rename(&e) {
        NoExclusiveRename::new(dst.to_path_buf(), e.to_string()).into()
    } else {
        anyhow::Error::new(e).context(pc_core::tf!(
            "не переместить {0} -> {1} (перенос обязан быть в пределах одного диска)",
            "cannot move {0} -> {1} (a move has to stay within one disk)",
            src.display(),
            dst.display()
        ))
    }
}

/// Tests only: what happens between the last look at a destination and the
/// move itself — another program creating a file there, or (with an `Err`)
/// the move being refused the way a volume refuses a call it lacks. Every
/// move of this crate passes through one bound rename (`bound::rename_bound`,
/// reached only through [`unit::move_unit`]), so a test on any of its
/// consumers can stage the race deterministically.
///
/// Compiled into this crate's own tests, and into the tests of a crate that
/// enables the `test-seams` feature (pc-api, whose jobs run on another
/// thread and so use [`race::before_move_under`]). Never in a release
/// build: the feature without debug assertions is a compile error.
#[cfg(any(test, feature = "test-seams"))]
#[doc(hidden)]
pub mod race {
    use std::cell::RefCell;
    use std::io;
    use std::path::Path;
    use std::sync::Mutex;

    type Shared = Box<dyn FnMut(&Path, &Path) -> io::Result<()> + Send>;

    /// Hooks for moves on any thread, each for sources whose path contains
    /// its marker — a folder name only that test uses.
    static UNDER: Mutex<Vec<(u64, String, Shared)>> = Mutex::new(Vec::new());
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// While the guard lives, `hook(src, dst)` runs before every move, on
    /// any thread, whose source path contains `marker`.
    pub fn before_move_under(
        marker: &str,
        hook: impl FnMut(&Path, &Path) -> io::Result<()> + Send + 'static,
    ) -> SharedGuard {
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        UNDER
            .lock()
            .unwrap()
            .push((id, marker.to_string(), Box::new(hook)));
        SharedGuard(id)
    }

    pub struct SharedGuard(u64);

    impl Drop for SharedGuard {
        fn drop(&mut self) {
            UNDER.lock().unwrap().retain(|(id, ..)| *id != self.0);
        }
    }

    type Hook = Box<dyn FnMut(&Path, &Path) -> io::Result<()>>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// While the guard lives, `hook(src, dst)` runs right before each move
    /// on this thread; an `Err` it returns is the move's error.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn before_move(hook: impl FnMut(&Path, &Path) -> io::Result<()> + 'static) -> Guard {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        Guard
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    /// Moments of a bound move that no check can cover (el-3wizg).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Syscall {
        /// After the last comparison, right before `renameat`.
        Before,
        /// Right after `renameat`, before what arrived is compared.
        After,
        /// Right after a compensating return, before anything is located.
        AfterReturn,
        /// Before an operation records what it moved (`src` and `dst` of
        /// its first item): after its last rename, before the journal.
        BeforeRecord,
    }

    type SyscallHook = Box<dyn FnMut(Syscall, &Path, &Path) -> io::Result<()>>;

    thread_local! {
        static SYSCALL: RefCell<Option<SyscallHook>> = const { RefCell::new(None) };
    }

    /// While the guard lives, `hook(moment, src, dst)` runs on this thread
    /// at both [`Syscall`] moments of every bound move; an `Err` at
    /// `Before` is the move's error, at `After` it is ignored.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn at_rename(
        hook: impl FnMut(Syscall, &Path, &Path) -> io::Result<()> + 'static,
    ) -> SyscallGuard {
        SYSCALL.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        SyscallGuard
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) struct SyscallGuard;

    impl Drop for SyscallGuard {
        fn drop(&mut self) {
            SYSCALL.with(|h| *h.borrow_mut() = None);
        }
    }

    type SharedSyscall = Box<dyn FnMut(Syscall, &Path, &Path) -> io::Result<()> + Send>;

    /// [`Syscall`] hooks for moves on any thread, each for sources whose
    /// path contains its marker (pc-api, whose jobs run on another thread).
    static UNDER_SYSCALL: Mutex<Vec<(u64, String, SharedSyscall)>> = Mutex::new(Vec::new());

    /// While the guard lives, `hook(moment, src, dst)` runs at every
    /// [`Syscall`] moment of every bound move, on any thread, whose source
    /// path contains `marker`.
    pub fn at_rename_under(
        marker: &str,
        hook: impl FnMut(Syscall, &Path, &Path) -> io::Result<()> + Send + 'static,
    ) -> SyscallSharedGuard {
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        UNDER_SYSCALL
            .lock()
            .unwrap()
            .push((id, marker.to_string(), Box::new(hook)));
        SyscallSharedGuard(id)
    }

    pub struct SyscallSharedGuard(u64);

    impl Drop for SyscallSharedGuard {
        fn drop(&mut self) {
            UNDER_SYSCALL
                .lock()
                .unwrap()
                .retain(|(id, ..)| *id != self.0);
        }
    }

    pub(crate) fn fire_syscall(at: Syscall, src: &Path, dst: &Path) -> io::Result<()> {
        SYSCALL.with(|h| match h.borrow_mut().as_mut() {
            Some(hook) => hook(at, src, dst),
            None => Ok(()),
        })?;
        let shown = src.to_string_lossy();
        for (_, marker, hook) in UNDER_SYSCALL.lock().unwrap().iter_mut() {
            if shown.contains(marker.as_str()) {
                hook(at, src, dst)?;
            }
        }
        Ok(())
    }

    pub(crate) fn fire(src: &Path, dst: &Path) -> io::Result<()> {
        HOOK.with(|h| match h.borrow_mut().as_mut() {
            Some(hook) => hook(src, dst),
            None => Ok(()),
        })?;
        let shown = src.to_string_lossy();
        for (_, marker, hook) in UNDER.lock().unwrap().iter_mut() {
            if shown.contains(marker.as_str()) {
                hook(src, dst)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod legacy;

#[cfg(test)]
mod race_tests;

#[cfg(all(test, unix))]
mod binding_tests;

#[cfg(all(test, unix))]
mod purge_tests;

#[cfg(all(test, unix))]
mod abandon_tests;

/// What `derived clean` leaves, with the reason — the one answer the
/// command line and the web preview both print.
///
/// There is no "selected" half: `derived clean` moves nothing — nothing of
/// Lightroom's (the user's decision of 2026-10-06), no system file and no
/// companion (the contract narrowed after el-2rpxq) — and this crate has no
/// forward move for a bundle at all (el-1bzcw: a refusal standing in front
/// of a working move is a move one edit away). What earlier versions put in
/// quarantine still comes home through `undo`; `purge` keeps it.
#[derive(Debug, Default)]
pub struct DerivedSelection {
    /// `(path, why)` for every bundle asked for, each left where it is.
    pub excluded: Vec<(String, String)>,
    /// `(what, why)` for what was asked for and is not even recorded.
    pub notes: Vec<(String, String)>,
}

/// Every present bundle of the asked kinds, at least `min_size` bytes, each
/// with why it stays. The reason is read from the kind and the path, never
/// from what a scan wrote down: a database scanned by an earlier version
/// holds bundles with nothing written against them (see `pc_core::derived`).
/// Asking for system junk adds one line that says why there is none: the
/// scan no longer records it, so without the line a person would see an
/// empty plan and no reason (el-2rpxq).
pub fn select_derived(
    db: &Db,
    kinds: &[pc_core::DerivedKind],
    min_size: Option<i64>,
) -> Result<DerivedSelection> {
    let mut out = DerivedSelection::default();
    for k in kinds {
        if *k == pc_core::DerivedKind::SystemJunk {
            out.notes.push(pc_core::derived::system_junk_note());
        }
        let f = pc_db::model::BundleFilter {
            kind: Some(*k),
            state: Some(BundleState::Present),
            min_size,
        };
        for b in db.list_bundles(&f)? {
            out.excluded.push((b.path.clone(), b.refusal()));
        }
    }
    Ok(out)
}

/// Carry a file the journal never claimed back out of quarantine.
///
/// Left by a database that is no longer here: this one has no row saying how
/// it got there, so it writes one now, and the move can be walked back like
/// any other.
pub fn adopt_orphan(db: &Db, run_id: i64, src: &str, dst: &str) -> Result<Tally> {
    // An early answer; the move itself never replaces what is there.
    if fs::symlink_metadata(dst).is_ok() {
        bail!(
            "{}",
            pc_core::tf!(
                "на месте уже лежит файл: {0}",
                "a file is already back in place: {0}",
                dst
            )
        );
    }
    let proof = files::evidence(Path::new(src))?;
    let size = proof.as_ref().and_then(|p| p.size).unwrap_or(0);
    let manifest = [pc_db::Moved {
        src: src.to_string(),
        dst: dst.to_string(),
        proof,
    }];
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "adopt",
        target_id: None,
        src,
        dst: Some(dst),
        size: size as i64,
        file_count: 1,
        manifest: &manifest,
    })?;
    let unit = unit::move_unit(
        &[unit::Member::forward(&manifest[0])],
        located::Way::Forward,
        None,
        &[],
    );
    let unit::Unit::Moved(arrived) = unit else {
        let r = unit::close_forward(db, jid, "adopt", Route::Restore, unit)?;
        let e = match r.stop {
            Some(stop) => stop.into(),
            None => anyhow::anyhow!("{}", r.why),
        };
        return Err(outcome::with_placed(e, r.placed, Route::Restore));
    };
    let done = Tally {
        files_back: 1,
        ..Default::default()
    };
    let closed = db.journal_close(
        jid,
        JournalStatus::Done,
        &pc_db::Event {
            moved: &manifest,
            ..pc_db::Event::new("adopt", "done")
        },
    );
    drop(arrived);
    if let Err(e) = closed {
        let e = stop_run(e, &done, Route::Restore, Vec::new());
        return Err(outcome::left_pending(e, jid, Route::Restore));
    }
    Ok(done)
}

/// "Delete for good" a file the journal never claimed: it is kept, and the
/// row says why ([`OrphanKept`], el-63ph1).
///
/// This used to remove whatever was at the path, recursively and by name —
/// a stranger's file, a folder with everything in it, a link, a Lightroom
/// catalogue. An orphan has no recorded evidence to prove anything against,
/// and purge deletes nothing it cannot prove, so nothing is deleted here at
/// all: not even a stop can leave part of it gone. `Ok` is the outcome,
/// journaled (`abandon`/`kept`, the row `failed`, so it claims nothing);
/// an error is a stop asked for before, or the journal not taking the row —
/// with nothing deleted either way.
pub fn abandon_orphan(
    db: &Db,
    run_id: i64,
    path: &str,
    control: &pc_core::work::Control,
) -> Result<OrphanKept> {
    control.current(path)?;
    // What is there now, without following a link: the words a person gets
    // must say what they would be deleting by hand.
    let now = fs::symlink_metadata(path).ok();
    let why = if is_lightroom(path) {
        OrphanWhy::Lightroom
    } else if now.as_ref().is_some_and(|m| m.is_dir()) {
        OrphanWhy::Folder
    } else {
        OrphanWhy::Unproven
    };
    let bytes = now.filter(|m| m.is_file()).map_or(0, |m| m.len());
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "abandon",
        target_id: None,
        src: path,
        dst: None,
        size: bytes as i64,
        file_count: 1,
        manifest: &[],
    })?;
    let kept = OrphanKept {
        entry: jid,
        path: path.to_string(),
        why,
        bytes,
    };
    let shown = kept.to_string();
    db.journal_close(
        jid,
        JournalStatus::Failed,
        &pc_db::Event {
            text: &shown,
            refused: &[(path.to_string(), kept.reason().to_string())],
            ..pc_db::Event::new("abandon", "kept")
        },
    )?;
    Ok(kept)
}

/// Permanently remove quarantined data older than `older_than_secs`.
///
/// This is the only destructive operation in the tool, and it deletes only
/// what it proves to be what each entry moved ([`purge_entry_controlled`]).
/// The totals are what actually went — including what an entry that
/// stopped after deleting had deleted (el-8s63g B2). An entry refused whole
/// is named in `skipped`, one that stopped after deleting in `stopped`,
/// each with why; a bundle, or anything of Lightroom's, in `kept`.
pub fn purge(db: &Db, older_than_secs: i64) -> Result<Totals> {
    let cutoff = pc_core::time::now_unix() - older_than_secs;
    let entries = db.journal_quarantined(Some(cutoff))?;
    let mut t = Totals::default();
    for e in entries {
        let done = match purge_entry(db, e.id) {
            Ok(done) => done,
            Err(err) if purge::purge_kept(&err).is_some() => {
                t.kept.push(format!("{err:#}"));
                Tally::default()
            }
            Err(err) if purge::purge_stopped(&err).is_some() => {
                t.stopped.push(format!("{} — {err:#}", e.src));
                purged_before(&err)
            }
            Err(err) => {
                t.skipped.push(format!("{} — {err:#}", e.src));
                Tally::default()
            }
        };
        t.bundles += done.purged_entries;
        t.files += done.purged_files;
        t.bytes += done.purged_bytes;
    }
    Ok(t)
}

/// Purge one previously reviewed quarantine entry.
pub fn purge_entry(db: &Db, id: i64) -> Result<Tally> {
    purge_entry_controlled(db, id, &pc_core::work::Control::default())
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

    /// A bundle as an earlier scan recorded it: nothing written against it.
    fn scanned(db: &Db, run: i64, dir: &Path, kind: pc_core::DerivedKind) -> pc_db::Bundle {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("cache"), b"cached").unwrap();
        db.upsert_bundle(
            &NewBundle {
                path: dir.display().to_string(),
                is_dir: true,
                disk: "root".into(),
                dev: 0,
                mount: dir.parent().unwrap().display().to_string(),
                kind,
                owner_ref: None,
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
    fn a_lightroom_bundle_an_old_scan_left_unblocked_is_left_and_named_by_the_core() {
        // A database scanned before el-126jk: previews with no block. Neither
        // the listing nor the move takes the saved verdict at its word.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        for (name, kind) in [
            ("Library Previews.lrdata", pc_core::DerivedKind::LrPreviews),
            ("PREVIEWS.LRDATA", pc_core::DerivedKind::LrDataOther),
            // Filed as junk by a bug or a hand: the path still says whose.
            ("Library Helper.lrdata", pc_core::DerivedKind::SystemJunk),
        ] {
            let dir = root.join(name);
            let b = scanned(&db, run, &dir, kind);
            assert!(b.blocked_code.is_none(), "{name}: scenario is wrong");
            let sel = select_derived(&db, &[kind], None).unwrap();
            let (_, why) = sel
                .excluded
                .iter()
                .find(|(p, _)| *p == b.path)
                .unwrap_or_else(|| panic!("{name}: not named as left"));
            assert!(why.contains("Lightroom is never touched"), "{why}");
            assert!(dir.join("cache").exists(), "{name} moved");
        }
        assert!(db.journal_quarantined(None).unwrap().is_empty());
    }

    #[test]
    fn system_junk_an_old_scan_recorded_is_left_and_named_whatever_its_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        fs::create_dir_all(&root).unwrap();
        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        let path = root.join(".DS_Store");
        // A database scanned by an earlier version, which recorded junk;
        // the bytes are a JPEG's. Neither they nor the row are asked.
        fs::write(&path, b"\xFF\xD8\xFF\xE0synthetic").unwrap();
        let md = fs::metadata(&path).unwrap();
        db.upsert_bundle(
            &NewBundle {
                path: path.display().to_string(),
                is_dir: false,
                disk: "root".into(),
                dev: 0,
                mount: root.display().to_string(),
                kind: pc_core::DerivedKind::SystemJunk,
                owner_ref: None,
                file_count: 1,
                size: md.len() as i64,
                newest_mtime: pc_core::time::mtime_unix(&md),
            },
            run,
        )
        .unwrap();
        let b = db.list_bundles(&Default::default()).unwrap().pop().unwrap();
        let sel = select_derived(&db, &[pc_core::DerivedKind::SystemJunk], None).unwrap();
        let (_, refused) = sel.excluded.iter().find(|(p, _)| *p == b.path).unwrap();
        assert!(refused.contains("moves no system files"), "{refused}");
        assert!(path.exists());
        assert!(db.journal_quarantined(None).unwrap().is_empty());
    }

    #[test]
    fn a_lightroom_bundle_moved_by_an_earlier_version_still_comes_home() {
        // Returning is putting back: the rule that keeps Lightroom where it
        // is must not keep it in quarantine either (el-126jk, decision 3).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("archive");
        let db = Db::open(&tmp.path().join("test.db")).unwrap();
        let run = db.start_run(&[root.display().to_string()], "test").unwrap();
        let dir = root.join("Library Previews.lrdata");
        let b = scanned(&db, run, &dir, pc_core::DerivedKind::LrPreviews);
        let jid = legacy::moved_by_an_earlier_version(&db, run, &b).unwrap();
        assert!(!dir.exists());
        let entry = db.journal_quarantined(None).unwrap().pop().unwrap();
        assert_eq!(entry.id, jid);

        undo(&db, entry.id).unwrap();
        assert_eq!(fs::read(dir.join("cache")).unwrap(), b"cached");
        assert_eq!(
            db.journal_entry(entry.id).unwrap().unwrap().status,
            pc_db::JournalStatus::Undone
        );
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
                        proof: None,
                    },
                    Moved {
                        src: src.with_extension("xmp").display().to_string(),
                        dst: dst.with_extension("xmp").display().to_string(),
                        proof: None,
                    },
                ],
            })
            .unwrap();
        db.journal_finish(id, JournalStatus::Done, None).unwrap();
        (id, src, dst)
    }

    #[test]
    fn an_undo_that_cannot_finish_moves_nothing_and_can_be_asked_again() {
        // Half an undo is not an undo. The photograph used to come home while
        // its sidecar could not follow — something already sat where it
        // belonged. Now the frame and its companions are one unit (user
        // decision (c)): nothing moves while one of them cannot, the entry
        // stays open with the reason, and asking again brings all of it.
        let tmp = tempfile::tempdir().unwrap();
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        let run = db.start_run(&[], "test").unwrap();
        let (id, src, dst) = quarantined(&db, run, tmp.path());
        let occupied = src.with_extension("xmp");
        fs::write(&occupied, b"newer edits").unwrap();

        let refused = undo(&db, id).unwrap_err();

        assert!(!src.exists(), "половина отката");
        assert!(dst.exists());
        assert!(dst.with_extension("xmp").exists(), "чужой файл затёрт");
        assert_eq!(fs::read(&occupied).unwrap(), b"newer edits");
        assert!(format!("{refused:#}").contains("xmp"), "{refused:#}");
        let entry = db.journal_entry(id).unwrap().unwrap();
        assert_eq!(
            entry.status,
            JournalStatus::Done,
            "частичный откат объявлен завершённым"
        );

        fs::remove_file(&occupied).unwrap();
        undo(&db, id).unwrap();
        assert_eq!(fs::read(&src).unwrap(), b"photo");
        assert_eq!(fs::read(&occupied).unwrap(), b"the edits that went with it");
        assert_eq!(
            db.journal_entry(id).unwrap().unwrap().status,
            JournalStatus::Undone
        );
    }
}
