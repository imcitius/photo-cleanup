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
pub use recovery::{
    reconcile, reconcile_undo, undo, undo_offered, undo_preview, Item, Reconciled, Standing,
};
pub use roots::RunRoots;

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

fn bundle_target(b: &Bundle, override_root: Option<&Path>) -> Result<Target> {
    let disk = disk_of(b);
    let src = PathBuf::from(&b.path);
    let rel = disk.relative(&src);

    match override_root {
        None => Target::beside(&src),
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
            Ok(gathered(root, &b.disk, &disk.mount, rel))
        }
    }
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

/// Where a bundle goes when quarantined. Only reads.
///
/// Beside itself by default, so the move is a rename and the directory is one
/// that already takes writes. `override_root` gathers everything in one place
/// instead, and is rejected unless it lives on the same device, because a
/// cross-device "move" would silently become a copy of the whole bundle.
pub fn quarantine_dest(b: &Bundle, override_root: Option<&Path>) -> Result<PathBuf> {
    Ok(bundle_target(b, override_root)?.dst)
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

/// Before a quarantine of bundles: every destination volume can move
/// without replacing. The command line and the web preview ask the same.
///
/// Only reads (el-23goa B1). A destination that cannot be worked out is the
/// answer, not something to skip: it refuses the run before the first move.
/// Bundles that never reach a move — kept by their kind, blocked, no longer
/// present — are left to `quarantine`, which says why for each.
pub fn check_bundles(bundles: &[Bundle], override_root: Option<&Path>) -> Result<()> {
    let mut dsts = Vec::new();
    for b in bundles {
        if !b.regenerable || b.blocked_code.is_some() || b.state != BundleState::Present {
            continue;
        }
        dsts.push(bundle_target(b, override_root)?.dst);
    }
    check_all(dsts.iter().map(PathBuf::as_path))
}

/// Before an apply of photographs: as [`check_bundles`]. Sidecars land
/// beside their photograph, in the same folder. A photograph that is already
/// gone is refused on its own by `quarantine_file`; any other doubt about a
/// destination refuses the run.
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

/// Before a reorganisation: as [`check_bundles`].
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
mod race_tests;

#[cfg(all(test, unix))]
mod binding_tests;

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

    let target = bundle_target(b, override_root)?;
    admit(&target)?;
    let dst = target.dst;
    let dst_str = dst.to_string_lossy().into_owned();
    // A bundle moves as one directory: one entry, with the evidence of which
    // directory it is, so its undo does not take a stranger for it either.
    let manifest = [pc_db::Moved {
        src: b.path.clone(),
        dst: dst_str.clone(),
        proof: files::evidence(Path::new(&b.path))?,
    }];
    let jid = db.journal_begin(&pc_db::NewJournalEntry {
        run_id,
        op: "quarantine",
        target_id: Some(b.id),
        src: &b.path,
        dst: Some(&dst_str),
        size: b.size,
        file_count: b.file_count,
        manifest: &manifest,
    })?;

    // One member: the bundle is one object (a folder of previews, or one
    // file), moved through the same unit as everything else.
    let unit = unit::move_unit(
        &[unit::Member::forward(&manifest[0])],
        located::Way::Forward,
        None,
        &[],
    );
    let unit::Unit::Moved(arrived) = unit else {
        let r = unit::close_forward(db, jid, "forward", Route::Quarantine, unit)?;
        if let Some(stop) = r.stop {
            return Err(outcome::with_placed(
                stop.into(),
                r.placed,
                Route::Quarantine,
            ));
        }
        return Err(outcome::with_placed(
            anyhow::anyhow!("{}", r.why),
            r.placed,
            Route::Quarantine,
        ));
    };
    let closed = db.journal_close(
        jid,
        JournalStatus::Done,
        &pc_db::Event {
            moved: &manifest,
            ..pc_db::Event::new("forward", "done")
        },
    );
    drop(arrived);
    if let Err(e) = closed {
        let e = e.context(pc_core::tf!(
            "{0} перенесён, но журнал не дописан: запись {1} осталась незавершённой — сверьте её",
            "{0} moved, but the journal was not completed: entry {1} is left pending — reconcile it",
            b.path,
            jid
        ));
        let e = stop_run(e, &Tally::bundle(b), Route::Quarantine, Vec::new());
        return Err(outcome::left_pending(e, jid, Route::Quarantine));
    }
    if let Err(e) = db.set_bundle_state(b.id, BundleState::Quarantined) {
        let e = e.context(pc_core::tf!(
            "{0} перенесён и записан в журнал, но индекс не обновлён",
            "{0} moved and is in the journal, but the index did not follow",
            b.path
        ));
        return Err(stop_run(
            e,
            &Tally::bundle(b),
            Route::Quarantine,
            Vec::new(),
        ));
    }
    Ok(Outcome::Moved)
}

/// What a quarantine of bundles did.
#[derive(Debug, Default)]
pub struct BundleReport {
    pub done: Tally,
    pub skipped: Vec<String>,
}

pub fn quarantine_many(
    db: &Db,
    run_id: i64,
    bundles: &[Bundle],
    override_root: Option<&Path>,
) -> Result<BundleReport> {
    let mut t = BundleReport::default();
    // A volume that cannot move without replacing stops the run before its
    // first move, not halfway through it.
    check_bundles(bundles, override_root)?;
    for b in bundles {
        match quarantine(db, run_id, b, override_root) {
            Ok(Outcome::Moved) => t.done.add(&Tally::bundle(b)),
            Ok(Outcome::Skipped) => t.skipped.push(pc_core::tf!(
                "{0} — изменился с момента сканирования",
                "{0} — changed since the scan",
                b.path
            )),
            Err(e) if is_run_stop(&e) || stopped_run(&e).is_some() => {
                let refused = t.skipped.clone();
                return Err(stop_run(e, &t.done, Route::Quarantine, refused));
            }
            Err(e) => t.skipped.push(format!("{} — {e}", b.path)),
        }
    }
    Ok(t)
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
            let shown = format!("{e:#}");
            db.journal_close(
                jid,
                JournalStatus::Failed,
                &pc_db::Event {
                    text: &shown,
                    error: Some(&shown),
                    ..pc_db::Event::new("abandon", "refused")
                },
            )?;
            Err(e)
        }
    }
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
    // A list this version cannot read says nothing about which files beside
    // the entry are its own; deleting by name would guess (el-1y8uo B2).
    recovery::readable(&e)?;
    // An entry with an object found away from its record, or whose place
    // could not be proven, is not deleted by its recorded paths: those may
    // name something else by now (el-lvtmk D6b). Deleting by proof is the
    // separate task el-3s9kp; until then such an entry is refused whole.
    if !e.located.is_empty() {
        bail!(
            "{}",
            pc_core::tf!(
                "запись {0}: место перенесённого не подтверждено по записанным путям ({1}); \
                 окончательное удаление по путям отказано, ничего не удалено",
                "entry {0}: what it moved is not proven to be at its recorded paths ({1}); \
                 permanent deletion by those paths is refused, nothing was deleted",
                id,
                e.located
                    .iter()
                    .map(|l| format!("{} — {}", l.src, l.at.shown()))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        );
    }
    let dst = e.dst.as_deref().context(pc_core::tr!(
        "в записи нет пути назначения",
        "the entry has no destination path"
    ))?;
    let path = Path::new(dst);
    control.check()?;
    db.journal_close(
        id,
        JournalStatus::Pending,
        &pc_db::Event {
            text: "Окончательное удаление начато; при прерывании часть файлов уже может отсутствовать",
            ..pc_db::Event::new("purge", "begun")
        },
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
