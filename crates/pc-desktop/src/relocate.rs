//! Moving the app's own data — the database and the thumbnail cache — to
//! another folder. Never the photographs.
//!
//! The shell stops the server, calls [`move_data`] ([`copy_data`], then
//! [`Copied::commit`]) and restarts only on `Ok`; everything that decides whether that is safe lives here, free of
//! Tauri, and is tested on every OS.
//!
//! The rules, each of which a test holds:
//!
//! - **Nothing is deleted from the source**, on success or on failure. The old
//!   folder stays where it was, whole, and remains the chosen one until the
//!   bootstrap is rewritten — which is the very last step.
//! - **Nobody else's database is overwritten.** A folder that already holds
//!   `photo-cleanup.db`, any SQLite sidecar (or `thumbs/`) is not a copy target; switching to it
//!   without copying is a separate, explicit action ([`switch_to_existing`]).
//! - **The copy is a consistent snapshot, not the files.** `VACUUM INTO`
//!   reads through the write-ahead log, so a database whose last writes are
//!   still in `-wal` is copied with them; the `-wal`/`-shm` files themselves
//!   are never copied byte for byte (half a checkpoint is a corrupt file).
//! - **The copy is proven before it is used**: integrity check, the same
//!   schema version, the same number of rows in every table; the thumbnails
//!   the same number of files and bytes. Only then is the private `.partial`
//!   directory's database published. Empty, exclusively created final
//!   sidecars reserve its namespace through bootstrap and the next launch.
//! - **One writer.** The database's writer lock is held for the whole copy,
//!   so the command line or a second server cannot write to the source
//!   between the snapshot and the switch.
//! - **Cleanup only owns its own namespace**: the private `.partial` directory
//!   and unchanged reservations this run created are removed, and only those — a `.partial` found there
//!   beforehand blocks the move instead of being cleaned up, because it is
//!   not ours to delete. Removal is never "check the name, then delete the
//!   name": the entry is first moved into a fresh private folder, and only
//!   what is proven there to be this run's own object is deleted (see
//!   [`remove_owned`] for the conditions and limits of that guarantee). What
//!   cannot be removed is returned as an error naming it; after a published
//!   copy that makes [`Copied::commit`] (and [`move_data`]) refuse to
//!   switch, so the shell does not restart as if cleanup had finished.

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::binding;
use crate::bootstrap::{
    read_bootstrap, write_bootstrap, write_bootstrap_checked, Binding, Bootstrap, Choice,
};
use crate::error::StartupError;
use crate::namespace::{self, Protected};
use crate::resolve::{choose_data_dir, NewDir, Source};
use crate::{DataLayout, SystemDirs, DB_FILE, THUMBS_DIR};

pub(crate) mod volume;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod storage_tests;

#[cfg(all(test, unix))]
mod rename_tests;

/// Room left over after the copy, on top of its own size.
///
/// The database copy is written by SQLite, which needs scratch space of its
/// own while it does, and the target disk is usually in use by something
/// else at the same time. 64 MiB is a few seconds of anything else writing
/// there; it is not meant to guarantee the disk stays usable afterwards,
/// only that the copy does not fail at 99 %.
pub const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

/// Exclusively created staging directory. Older versions used a file here;
/// both that file and its adjacent SQLite sidecars still block relocation.
pub const PARTIAL_DB: &str = "photo-cleanup.db.partial";
/// The thumbnail copy while it is not yet proven.
pub const PARTIAL_THUMBS: &str = "thumbs.partial";

const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// How much there is to move.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DataSize {
    /// The database with its write-ahead log — what the snapshot reads.
    pub db_bytes: u64,
    pub thumbs_bytes: u64,
    pub thumbs_files: u64,
}

impl DataSize {
    pub fn total(&self) -> u64 {
        self.db_bytes.saturating_add(self.thumbs_bytes)
    }
}

/// The size of a data folder's contents, as they are on disk now.
pub fn measure(layout: &DataLayout) -> io::Result<DataSize> {
    let mut db_bytes = 0u64;
    for p in [layout.db.clone(), sidecar(&layout.db, "-wal")] {
        match fs::metadata(&p) {
            Ok(m) => db_bytes = db_bytes.saturating_add(m.len()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let (thumbs_files, thumbs_bytes) = tree_size(&layout.thumbs)?;
    Ok(DataSize {
        db_bytes,
        thumbs_bytes,
        thumbs_files,
    })
}

/// Why a folder cannot receive a copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Blocker {
    /// A relative path: its meaning depends on where the app was started.
    RelativePath,
    /// It is the current data folder.
    SameFolder,
    /// It is inside the current data folder's thumbnail cache — the copy
    /// would copy into itself.
    InsideCurrent,
    /// It exists and is not a folder.
    NotADirectory,
    /// It already holds a database. Not overwritten; switching to it without
    /// copying is [`switch_to_existing`].
    DatabaseExists,
    /// A SQLite companion name is occupied, even without the database.
    SidecarExists {
        path: PathBuf,
    },
    /// It already holds a thumbnail cache. Not merged into.
    ThumbsExist,
    /// A `.partial` left there by something else — not ours to remove.
    LeftoverPartial {
        path: PathBuf,
    },
    NotEnoughSpace {
        needed: u64,
        available: u64,
    },
    /// Free space could not be read.
    SpaceUnknown {
        reason: String,
    },
    /// The volume that would hold `path` cannot keep this run's temporary
    /// folders private to this user (no owners, noowners mount, no ACLs,
    /// or not known): `reason` says which, naming the volume. Refused
    /// before anything is written there ([`volume`]).
    NoPrivateFolders {
        path: PathBuf,
        reason: String,
    },
    /// The volume that would hold `path` cannot rename an entry without
    /// replacing whatever is at the new name (macOS exFAT: `ENOTSUP`), or
    /// does not say it can (`reason`, naming the volume or the error).
    /// Publishing the copy and removing this run's own leftovers both rest
    /// on that call, and nothing replaces it safely, so the move is refused
    /// before anything is written there that would have to be cleaned up
    /// (el-21zyg).
    NoExclusiveRename {
        path: PathBuf,
        reason: String,
    },
    /// The data folder is fixed for this launch (portable marker or
    /// `--data-dir`); the bootstrap would not be read.
    ModeFixed {
        source: Source,
    },
    /// `path`, the move's `role` folder, is not in a protected namespace
    /// ([`crate::namespace`]): the folder `component` on its path could be
    /// renamed, replaced or re-permissioned by somebody other than this
    /// user and the system (`reason` says how), or this system cannot prove
    /// that it could not. Refused before anything is written anywhere.
    UnprotectedFolder {
        role: FolderRole,
        path: PathBuf,
        component: PathBuf,
        reason: String,
    },
    /// A file the move would write by name in the current data folder (the
    /// database, SQLite's `-wal`/`-shm`/`-journal`, the writer lock) could
    /// pass the write on to something else — a link, a second name, another
    /// owner — or can be changed by other users (`reason` says which).
    /// Refused before anything is written anywhere; the file is left as it
    /// is.
    UnsafeFile {
        path: PathBuf,
        reason: String,
    },
}

/// Which folder of a move a [`Blocker::UnprotectedFolder`] is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderRole {
    /// Where the copy would go.
    Target,
    /// The current data folder: the source database, its SQLite files and
    /// its writer lock.
    Source,
    /// The per-user app folder holding the bootstrap.
    Settings,
}

/// What moving to `target` would do, and whether it may.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MovePreview {
    pub from: PathBuf,
    pub to: PathBuf,
    pub size: DataSize,
    /// The copy's size plus [`SPACE_MARGIN`].
    pub needed: u64,
    pub available: Option<u64>,
    /// Empty: the copy may go ahead.
    pub blockers: Vec<Blocker>,
    /// The blockers in words, in the interface's language, for the screen.
    pub reasons: Vec<String>,
    /// The target holds a database one could switch to instead.
    pub existing_database: bool,
}

/// Look, touch nothing: what copying the data at `from` to `target` means.
pub fn preview_move(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    target: &Path,
) -> MovePreview {
    preview_move_with(dirs, from, source, target, pc_core::disk::available_space)
}

/// [`preview_move`] with the free-space probe handed in, for tests.
pub fn preview_move_with(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> MovePreview {
    let size = measure(from).unwrap_or_default();
    let needed = size.total().saturating_add(SPACE_MARGIN);
    let mut blockers = Vec::new();
    let mut free = None;
    if matches!(source, Source::Portable | Source::Override) {
        blockers.push(Blocker::ModeFixed { source });
    }
    let existing_database = fs::symlink_metadata(target.join(DB_FILE)).is_ok_and(|m| m.is_file());
    if !target.is_absolute() {
        blockers.push(Blocker::RelativePath);
    } else if same_place(target, &from.dir) {
        blockers.push(Blocker::SameFolder);
    } else if under(target, &from.thumbs) {
        blockers.push(Blocker::InsideCurrent);
    } else if occupied(target) && !target.is_dir() {
        blockers.push(Blocker::NotADirectory);
    } else {
        if occupied(&target.join(DB_FILE)) {
            blockers.push(Blocker::DatabaseExists);
        }
        for suffix in SIDECARS {
            let path = sidecar(&target.join(DB_FILE), suffix);
            if occupied(&path) {
                blockers.push(Blocker::SidecarExists { path });
            }
        }
        if occupied(&target.join(THUMBS_DIR)) {
            blockers.push(Blocker::ThumbsExist);
        }
        for p in legacy_partials(target) {
            if occupied(&p) {
                blockers.push(Blocker::LeftoverPartial { path: p });
            }
        }
        let volume = volume::check_target(target);
        if let Err(reason) = &volume {
            blockers.push(Blocker::NoPrivateFolders {
                path: target.to_path_buf(),
                reason: reason.clone(),
            });
        }
        if let Err(reason) = volume::check_exclusive_rename(target) {
            blockers.push(Blocker::NoExclusiveRename {
                path: target.to_path_buf(),
                reason,
            });
        }
        // The same proof the move itself starts with ([`Spaces::admit`]):
        // read only, nothing is created or opened for writing.
        blockers.extend(Spaces::refusals(dirs, from, target, volume.is_ok()));
        match available(target) {
            Ok(a) => {
                free = Some(a);
                if a < needed {
                    blockers.push(Blocker::NotEnoughSpace {
                        needed,
                        available: a,
                    });
                }
            }
            Err(e) => blockers.push(Blocker::SpaceUnknown {
                reason: e.to_string(),
            }),
        }
    }
    let reasons = blockers.iter().map(ToString::to_string).collect();
    MovePreview {
        reasons,
        from: from.dir.clone(),
        to: target.to_path_buf(),
        size,
        needed,
        available: free,
        blockers,
        existing_database,
    }
}

/// What a finished, verified copy holds.
#[derive(Debug, Serialize)]
pub struct Copied {
    // Keep both archives exclusive until the bootstrap has been committed.
    #[serde(skip)]
    _source_lock: pc_core::lock::WriterLock,
    #[serde(skip)]
    _target_lock: pc_core::lock::WriterLock,
    /// The folder the copy was published into; the bootstrap names
    /// `layout.dir` only while that path still leads here.
    #[serde(skip)]
    target_dir: TargetDir,
    /// The target's protected path, the folders on it held open.
    #[serde(skip)]
    target_ns: Protected,
    /// The current data folder and the settings folder, admitted before the
    /// first write and held until the bootstrap is written.
    #[serde(skip)]
    spaces: Spaces,
    /// The proven database and thumbnail folder, as objects.
    #[serde(skip)]
    payload: Payload,
    #[serde(skip)]
    sidecars: SidecarReservations,
    #[serde(skip)]
    dirs: SystemDirs,
    #[serde(skip)]
    source: Source,
    pub layout: DataLayout,
    pub tables: usize,
    pub rows: u64,
    pub thumbs_files: u64,
    pub thumbs_bytes: u64,
    /// The copy is published and proven, but the now-empty staging folder
    /// could not be removed; why, and where it is (either the original
    /// `.partial` name or a `.photo-cleanup-removing-*` folder it was moved
    /// into). [`Copied::commit`] refuses such a copy with
    /// [`RelocateError::Incomplete`], so the caller cannot switch and
    /// restart as if cleanup had finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staging_left: Option<String>,
}

/// The proven copy, as the objects that were proven — not as names. Taken
/// in the private staging folder, carried through publication and checked
/// against the target's names before the bootstrap may name them
/// ([`Copied::commit`]).
#[derive(Debug)]
struct Payload {
    /// The database file this run created exclusively, filled by SQLite
    /// and verified through this very object ([`snapshot`]).
    db: same_file::Handle,
    /// `thumbs/`, made by this run in the staging folder (empty when the
    /// source has no cache) and counted through its descriptor.
    thumbs: same_file::Handle,
    /// Written onto `db` before it was verified; recorded in the binding.
    generation: String,
}

/// The source and settings folders of a move, admitted as protected
/// namespaces ([`crate::namespace`]) before anything is written.
#[derive(Debug)]
struct Spaces {
    source: Protected,
    settings: Protected,
}

impl Spaces {
    /// Every refusal for the three folders of a move, for the preview. The
    /// target is only judged here if its volume passed (`volume_ok`): a
    /// volume without owners is already named by [`Blocker::NoPrivateFolders`].
    fn refusals(
        dirs: &SystemDirs,
        from: &DataLayout,
        target: &Path,
        volume_ok: bool,
    ) -> Vec<Blocker> {
        let mut out = Vec::new();
        if volume_ok {
            if let Err(b) = admit_target(target) {
                out.push(b);
            }
        }
        if let Err(b) = Self::admit(dirs, from) {
            out.extend(b);
        }
        out
    }

    fn admit(dirs: &SystemDirs, from: &DataLayout) -> Result<Self, Vec<Blocker>> {
        let source = namespace::admit_existing(&from.dir)
            .map_err(|r| vec![unprotected(FolderRole::Source, r)]);
        let source = source.and_then(|source| {
            let leaves = source_files(&source);
            if leaves.is_empty() {
                Ok(source)
            } else {
                Err(leaves)
            }
        });
        let settings = namespace::admit(&dirs.app_local_data)
            .map_err(|r| vec![unprotected(FolderRole::Settings, r)]);
        match (source, settings) {
            (Ok(source), Ok(settings)) => Ok(Self { source, settings }),
            (source, settings) => Err([source.err(), settings.err()]
                .into_iter()
                .flatten()
                .flatten()
                .collect()),
        }
    }

    fn recheck(&self) -> Result<(), RelocateError> {
        self.source
            .recheck()
            .map_err(|r| blocked(unprotected(FolderRole::Source, r)))?;
        self.settings
            .recheck()
            .map_err(|r| blocked(unprotected(FolderRole::Settings, r)))
    }
}

/// The files the move opens for writing by name in the current data
/// folder: the writer lock (its note is written into it), and the database
/// with SQLite's `-wal`, `-shm` and `-journal` (reading a snapshot opens and
/// may write them; closing checkpoints into the database). The folder being
/// protected does not make these safe: a symbolic link or a second name
/// already there would carry the write to somebody else's file. Each must
/// be absent or this user's plain single-name file, the SQLite ones also
/// not writable by anybody else ([`namespace::check_file_at`]). Read only.
fn source_files(source: &Protected) -> Vec<Blocker> {
    let Some(folder) = source.dir() else {
        return Vec::new();
    };
    let names = [
        (DB_FILE.to_string(), namespace::FileUse::Contents),
        (format!("{DB_FILE}-wal"), namespace::FileUse::Contents),
        (format!("{DB_FILE}-shm"), namespace::FileUse::Contents),
        (format!("{DB_FILE}-journal"), namespace::FileUse::Contents),
        (format!("{DB_FILE}.writer-lock"), namespace::FileUse::Note),
    ];
    names
        .into_iter()
        .filter_map(|(name, what)| {
            let path = source.path().join(&name);
            namespace::check_file_at(folder, &name, what)
                .err()
                .map(|reason| Blocker::UnsafeFile { path, reason })
        })
        .collect()
}

/// Admit the target as a protected namespace whose volume has a stable
/// identity (the binding records it).
fn admit_target(target: &Path) -> Result<Protected, Blocker> {
    let admitted = namespace::admit(target).map_err(|r| unprotected(FolderRole::Target, r))?;
    let deepest = admitted.deepest();
    if let Err(e) = namespace::volume_id(deepest.1) {
        return Err(Blocker::UnprotectedFolder {
            role: FolderRole::Target,
            path: target.to_path_buf(),
            component: deepest.0.to_path_buf(),
            reason: pc_core::tf!(
                "у тома нет постоянного идентификатора, к которому можно привязать копию: {0}",
                "its volume has no lasting identity the copy could be bound to: {0}",
                e
            ),
        });
    }
    Ok(admitted)
}

fn unprotected(role: FolderRole, r: namespace::Refusal) -> Blocker {
    Blocker::UnprotectedFolder {
        role,
        path: r.path,
        component: r.component,
        reason: r.reason,
    }
}

fn blocked(blocker: Blocker) -> RelocateError {
    RelocateError::Blocked {
        blockers: vec![blocker],
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum RelocateError {
    /// The preview found a reason not to (it is repeated right before copying).
    Blocked { blockers: Vec<Blocker> },
    /// Another process is writing to the source database.
    Locked { reason: String },
    /// Reading the source or writing the target failed.
    Copy { reason: String },
    /// The copy was made but is not the same data.
    Verify { reason: String },
    /// Recording the new folder failed; the old one is still the chosen one.
    Startup { error: StartupError },
    /// The run failed with `error`, and its cleanup could not remove all it
    /// had created. `left` names each leftover and why. Nothing that was not
    /// proven to be this run's own was deleted.
    Cleanup {
        error: Box<RelocateError>,
        left: Vec<String>,
    },
    /// The copy in `copy` is published and proven, but this run's own
    /// staging could not be removed (`left`: where and why). The bootstrap
    /// was not changed: the current folder is still the chosen one, and the
    /// copy is kept, not rolled back.
    ///
    /// Only returned after `copy` was checked to still be the folder the
    /// proven copy was moved into, through its handle.
    Incomplete { copy: PathBuf, left: String },
    /// The proven copy was published into the folder opened as `target`,
    /// but `target` no longer leads to that folder: someone renamed or
    /// replaced it (`reason` says where the folder is now, if known).
    /// Nothing at `target` was touched and the bootstrap was not changed.
    Displaced { target: PathBuf, reason: String },
}

impl From<StartupError> for RelocateError {
    fn from(error: StartupError) -> Self {
        Self::Startup { error }
    }
}

impl std::error::Error for RelocateError {}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::RelativePath => {
                pc_core::tr!("нужен полный путь", "a full path is needed").to_string()
            }
            Self::SameFolder => pc_core::tr!(
                "это и есть текущая папка данных",
                "this is the current data folder"
            )
            .to_string(),
            Self::InsideCurrent => pc_core::tr!(
                "папка внутри кэша превью текущей папки данных",
                "the folder is inside the current thumbnail cache"
            )
            .to_string(),
            Self::NotADirectory => pc_core::tr!("это не папка", "it is not a folder").to_string(),
            Self::DatabaseExists => pc_core::tf!(
                "в папке уже есть {0}; она не перезаписывается",
                "the folder already holds {0}; it is not overwritten",
                DB_FILE
            ),
            Self::SidecarExists { path } => pc_core::tf!(
                "имя файла SQLite {0} уже занято; чужие файлы не изменяются",
                "SQLite file name {0} is already occupied; existing files are preserved",
                path.display()
            ),
            Self::ThumbsExist => pc_core::tf!(
                "в папке уже есть {0}/; он не перезаписывается",
                "the folder already holds {0}/; it is not overwritten",
                THUMBS_DIR
            ),
            Self::LeftoverPartial { path } => pc_core::tf!(
                "в папке осталась чужая незавершённая копия {0}; удалите её сами, если она не нужна",
                "an unfinished copy {0} is already there; remove it yourself if it is not needed",
                path.display()
            ),
            Self::NotEnoughSpace { needed, available } => pc_core::tf!(
                "не хватает места: нужно {0}, свободно {1}",
                "not enough space: {0} needed, {1} free",
                pc_core::bytes::fmt_bytes(*needed),
                pc_core::bytes::fmt_bytes(*available)
            ),
            Self::SpaceUnknown { reason } => pc_core::tf!(
                "не узнать свободное место: {0}",
                "cannot read the free space: {0}",
                reason
            ),
            Self::NoPrivateFolders { path, reason } => pc_core::tf!(
                "в {0} нельзя создать временные папки переноса, закрытые от других \
                 пользователей: {1}. Выберите папку на другом томе",
                "{0} cannot hold the move's temporary folders private to this user: {1}. \
                 Choose a folder on another volume",
                path.display(),
                reason
            ),
            Self::NoExclusiveRename { path, reason } => pc_core::tf!(
                "в {0} нельзя перенести данные: том не умеет переименовывать без замены \
                 существующего ({1}), а без этого перенос не может ни опубликовать копию, \
                 ни безопасно убрать за собой. Выберите папку на другом томе",
                "{0} cannot receive the data: its volume cannot rename without replacing \
                 ({1}), and without that the move can neither publish the copy nor safely \
                 remove its own leftovers. Choose a folder on another volume",
                path.display(),
                reason
            ),
            Self::UnprotectedFolder {
                role,
                path,
                component,
                reason,
            } => {
                let (ru, en) = match role {
                    FolderRole::Target => ("папка назначения", "the target folder"),
                    FolderRole::Source => ("текущая папка данных", "the current data folder"),
                    FolderRole::Settings => ("папка настроек программы", "the app's settings folder"),
                };
                let at = if component == path {
                    String::new()
                } else {
                    format!(" {}", component.display())
                };
                pc_core::tf!(
                    "{0} {1} лежит там, где путь могут подменить или перенастроить другие \
                     пользователи:{2} {3}. Выберите папку, путь к которой принадлежит только \
                     вам и системе, например внутри домашней папки",
                    "{0} {1} is on a path other users could redirect or re-permission:{2} {3}. \
                     Choose a folder whose whole path belongs only to you and the system, \
                     for example inside your home folder",
                    pc_core::tr!(ru, en),
                    path.display(),
                    at,
                    reason
                )
            }
            Self::UnsafeFile { path, reason } => pc_core::tf!(
                "перенос записывает в {0}, но {1}. Файл не тронут; уберите или замените его \
                 и повторите",
                "the move writes to {0}, but {1}. The file was left untouched; move it away \
                 or replace it, then try again",
                path.display(),
                reason
            ),
            Self::ModeFixed { source } => match source {
                Source::Portable => pc_core::tr!(
                    "включён переносной режим: папка данных — рядом с программой",
                    "portable mode is on: the data folder is next to the program"
                )
                .to_string(),
                _ => pc_core::tr!(
                    "папка данных задана при запуске (--data-dir)",
                    "the data folder was given at start-up (--data-dir)"
                )
                .to_string(),
            },
        };
        f.write_str(&s)
    }
}

impl fmt::Display for RelocateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Blocked { blockers } => {
                let list: Vec<String> = blockers.iter().map(ToString::to_string).collect();
                pc_core::tf!(
                    "перенос невозможен: {0}",
                    "cannot move: {0}",
                    list.join("; ")
                )
            }
            Self::Locked { reason } => {
                pc_core::tf!("база занята: {0}", "the database is in use: {0}", reason)
            }
            Self::Copy { reason } => pc_core::tf!(
                "копирование не удалось: {0}",
                "the copy failed: {0}",
                reason
            ),
            Self::Verify { reason } => pc_core::tf!(
                "копия не совпала с исходной: {0}",
                "the copy does not match the original: {0}",
                reason
            ),
            Self::Startup { error } => error.to_string(),
            Self::Cleanup { error, left } => pc_core::tf!(
                "{0}; не удалось убрать за собой: {1}",
                "{0}; cleanup left: {1}",
                error,
                left.join("; ")
            ),
            Self::Incomplete { copy, left } => pc_core::tf!(
                "копия в {0} готова и проверена, но её временная папка не убрана: {1}. \
                 Папка данных не переключена. Удалите остаток вручную и выберите {0} \
                 как существующую папку",
                "the copy in {0} is complete and verified, but its staging folder was not \
                 removed: {1}. The data folder was not switched. Remove the leftover by hand, \
                 then choose {0} as an existing folder",
                copy.display(),
                left
            ),
            Self::Displaced { target, reason } => pc_core::tf!(
                "проверенная копия опубликована в папке, открытой как {0}, но {1}. \
                 Папка данных не переключена; то, что лежит по пути {0}, не является \
                 проверенной копией, пока вы не убедились в этом сами",
                "the verified copy was published in the folder opened as {0}, but {1}. \
                 The data folder was not switched; whatever is at {0} is not the verified \
                 copy unless you have made sure of it yourself",
                target.display(),
                reason
            ),
        };
        f.write_str(&s)
    }
}

/// Copy the database and the thumbnails from `from` into `target`, and prove
/// the copy. The source is only read.
///
/// The caller has stopped its own server first: the writer lock taken here
/// keeps other processes out, not the caller's own threads.
pub fn copy_data(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Result<Copied, RelocateError> {
    copy_data_with(dirs, from, source, target, available, &mut no_race)
}

/// What the desktop shell runs for "copy to a new folder": [`copy_data`],
/// then [`Copied::commit`]. `Ok` means the bootstrap now names `target` and
/// every piece of this run's staging is gone, so restarting is honest; any
/// leftover comes back as an error ([`RelocateError::Incomplete`]) instead.
pub fn move_data(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Result<(), RelocateError> {
    move_data_with(dirs, from, source, target, available, &mut no_race)
}

fn move_data_with(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<(), RelocateError> {
    copy_data_with(dirs, from, source, target, available, race)?.commit()
}

/// The move, admission first.
///
/// Before the writer lock file, a reservation, a staging folder, the target
/// folder or the bootstrap is created, the three folders the move writes in
/// — the target, the current data folder (writer lock, SQLite's files) and
/// the settings folder (bootstrap) — are admitted as protected namespaces
/// ([`crate::namespace`]) and held. Anything not proven is refused with the
/// preview's blocker, and nothing at all has been written anywhere. From
/// then on every path used is the canonical one that was proven.
fn copy_data_with(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<Copied, RelocateError> {
    let preview = preview_move_with(dirs, from, source, target, &available);
    if !preview.blockers.is_empty() {
        return Err(RelocateError::Blocked {
            blockers: preview.blockers,
        });
    }
    let spaces =
        Spaces::admit(dirs, from).map_err(|blockers| RelocateError::Blocked { blockers })?;
    let target_ns = admit_target(target).map_err(blocked)?;
    let from = &DataLayout::in_dir(spaces.source.path());
    let source_lock = pc_core::lock::take_writer(
        &from.db,
        &format!("moving the app data to {}", target.display()),
    )
    .map_err(|e| RelocateError::Locked {
        reason: format!("{e:#}"),
    })?;
    // Measured again under the lock: this is the size the copy must match.
    let size = measure(from).map_err(copy_err)?;
    let mut cleanup = Cleanup::new(target_ns);
    let staged = match stage(dirs, from, source, &available, size, &mut cleanup, race) {
        Ok(staged) => staged,
        Err(error) => {
            // `stage` has released the target lock by now, so an empty
            // folder this run created can go too.
            let left = cleanup.abort();
            return Err(match (error, left.is_empty()) {
                (error, true) => error,
                // Already a cleanup report (the rename probe's): one list.
                (
                    RelocateError::Cleanup {
                        error,
                        left: mut first,
                    },
                    false,
                ) => {
                    first.extend(left);
                    RelocateError::Cleanup { error, left: first }
                }
                (error, false) => RelocateError::Cleanup {
                    error: Box::new(error),
                    left,
                },
            });
        }
    };
    let staging_left = cleanup.succeed_with(race);
    let mut sidecars = cleanup.sidecars.take().unwrap_or_default();
    sidecars.keep = true;
    let target_ns = cleanup.target.take().expect("stage keeps the target");
    Ok(Copied {
        _source_lock: source_lock,
        _target_lock: staged.target_lock,
        target_dir: staged.target_dir,
        target_ns,
        spaces,
        payload: staged.payload,
        sidecars,
        dirs: dirs.clone(),
        source,
        layout: staged.layout,
        tables: staged.tables,
        rows: staged.rows,
        thumbs_files: staged.files,
        thumbs_bytes: staged.bytes,
        staging_left,
    })
}

/// A published copy, before `copy_data` hands it over.
struct Staged {
    target_lock: pc_core::lock::WriterLock,
    target_dir: TargetDir,
    layout: DataLayout,
    payload: Payload,
    tables: usize,
    rows: u64,
    files: u64,
    bytes: u64,
}

/// Everything `copy_data` does in the target. Whatever it creates is
/// recorded in `cleanup` as soon as it exists, so the caller can undo it.
///
/// The target's path is protected ([`copy_data_with`]): no other account
/// can rename or replace anything on it. On top of that, what decides *what*
/// is published is never a name (reviews el-2ztq8, el-59w6z):
///
/// - missing folders of the target are made one at a time relative to the
///   open parent and checked ([`Protected::create_missing`]); the target is
///   then held open ([`TargetDir`]) and the staging folder is accepted only
///   as the entry `photo-cleanup.db.partial` *in that open folder*;
/// - the database file is created by this run, exclusively, in the open
///   staging folder, and SQLite may fill it only while the path leads to
///   that very file; it is verified through that object ([`snapshot`]);
/// - the thumbnails are copied into `thumbs/` made in the open staging
///   folder and counted through descriptors;
/// - publication moves exactly these objects out of the open staging folder
///   into the open target folder, without replacement
///   ([`anchored::move_child`]), and the objects travel on as the
///   [`Payload`] that [`Copied::commit`] checks before the bootstrap names
///   them.
fn stage(
    dirs: &SystemDirs,
    from: &DataLayout,
    source: Source,
    available: impl Fn(&Path) -> io::Result<u64>,
    size: DataSize,
    cleanup: &mut Cleanup,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<Staged, RelocateError> {
    let target_ns = cleanup.target.as_mut().expect("the target is admitted");
    target_ns
        .create_missing()
        .map_err(|r| blocked(unprotected(FolderRole::Target, r)))?;
    let target = target_ns.path().to_path_buf();
    let target_dir = TargetDir::from_file(
        target_ns
            .dir()
            .expect("created")
            .try_clone()
            .map_err(copy_err)?,
    )
    .map_err(copy_err)?;
    // Before the first file this run would have to remove again — the
    // writer lock, the reservations, the staging folder: on a volume that
    // cannot rename without replacing, the cleanup could not move them
    // aside either, and they would block every retry (el-21zyg).
    probe_exclusive_rename(&target, target_dir.file())?;
    let to = DataLayout::in_dir(&target);
    let target_lock = pc_core::lock::take_writer(&to.db, "receiving app data").map_err(|e| {
        RelocateError::Locked {
            reason: format!("{e:#}"),
        }
    })?;
    // Another process may have populated the target since the first preview.
    let preview = preview_move_with(dirs, from, source, &target, &available);
    if !preview.blockers.is_empty() {
        return Err(RelocateError::Blocked {
            blockers: preview.blockers,
        });
    }
    // A create_new claim for every final sidecar closes the gap after the
    // preview (including its space probe). Keep these empty files across
    // publication/bootstrap/restart: deleting them would reopen that gap.
    cleanup
        .sidecars
        .insert(SidecarReservations::default())
        .claim(&to.db)
        .map_err(copy_err)?;
    // Claim a directory, not only the SQLite main file. SQLite is allowed
    // to create/remove companions only inside this private namespace.
    let partial = target.join(PARTIAL_DB);
    let staging = &*cleanup
        .db
        .insert(OwnedDirectory::create_with(&partial, race).map_err(copy_err)?);
    // The folder registered by name must be the entry of the open target.
    if !target_dir.holds(PARTIAL_DB, &staging.handle) {
        return Err(copy_err(pc_core::tf!(
            "{0} был создан этим переносом, но папка {1} больше не та, что открыта \
             для переноса; ничего не опубликовано",
            "{0} was created by this run, but the folder {1} is no longer the one \
             opened for the move; nothing was published",
            partial.display(),
            target.display()
        )));
    }
    reject_legacy_sidecars(&target)?;
    race(Step::Registered, &partial);
    let snap = snapshot(&from.db, &partial, staging, race)?;

    let staging_dir = staging.handle.as_file();
    let (files, bytes, thumbs) = stage_thumbs(&from.thumbs, staging_dir).map_err(copy_err)?;
    if (files, bytes) != (size.thumbs_files, size.thumbs_bytes) {
        return Err(RelocateError::Verify {
            reason: format!(
                "thumbnails: {} files, {} bytes copied; {} files, {} bytes expected",
                files, bytes, size.thumbs_files, size.thumbs_bytes
            ),
        });
    }

    // The proven copies take their real names: thumbnails first, the
    // database last, so a target with `photo-cleanup.db` in it is always a
    // complete one.
    if let Some(sidecars) = &mut cleanup.sidecars {
        sidecars.ensure_owned().map_err(copy_err)?;
    }
    reject_legacy_sidecars(&target)?;
    race(Step::Verified, &partial);
    target_dir.still_at(&target).map_err(copy_err)?;
    if let Some(ns) = &cleanup.target {
        ns.recheck()
            .map_err(|r| blocked(unprotected(FolderRole::Target, r)))?;
    }
    race(Step::Publish, &partial);
    let staging_dir = cleanup.db.as_ref().expect("registered").handle.as_file();
    anchored::move_child(staging_dir, THUMBS_DIR, &thumbs, target_dir.file())
        .map_err(|e| publish_err(&partial, THUMBS_DIR, &target, &e))?;
    // Published, but ours until the database is: a failure below removes
    // it again, by identity.
    let kept = same_file::Handle::from_file(thumbs.as_file().try_clone().map_err(copy_err)?)
        .map_err(copy_err)?;
    cleanup.thumbs = Some(OwnedDirectory {
        path: to.thumbs.clone(),
        handle: thumbs,
        keep: false,
    });
    anchored::move_child(staging_dir, DB_FILE, &snap.db, target_dir.file())
        .map_err(|e| publish_err(&partial, DB_FILE, &target, &e))?;
    sync_dir(target_dir.file());
    Ok(Staged {
        target_lock,
        target_dir,
        layout: to,
        payload: Payload {
            db: snap.db,
            thumbs: kept,
            generation: snap.generation,
        },
        tables: snap.tables,
        rows: snap.rows,
        files,
        bytes,
    })
}

/// Publication of `name` from the staging folder failed; nothing at any
/// name in the target was replaced.
fn publish_err(partial: &Path, name: &str, target: &Path, e: &io::Error) -> RelocateError {
    copy_err(pc_core::tf!(
        "{0} из временной папки этого переноса ({1}) не опубликован в {2}: {3}; \
         ничего не заменено",
        "{0} from this run's staging folder ({1}) was not published in {2}: {3}; \
         nothing was replaced",
        name,
        partial.display(),
        target.display(),
        e
    ))
}

/// Try the call everything of this run is moved with — a rename that never
/// replaces ([`at::rename_no_replace`]) — inside the open `target`, before
/// anything is written there that the run would have to remove again
/// (el-21zyg). [`volume::check_exclusive_rename`] already asked the volume;
/// this catches a volume that does not say (Linux has no such query) or
/// says wrongly.
///
/// The trial runs in a fresh private folder ([`Aside::create`]): an empty
/// file is created in it exclusively and renamed within it. Whatever the
/// answer, the file is deleted only from that folder's descriptor and only
/// while the entry is the file just made, then the folder is removed as
/// the cleanup removes its own ([`Aside::remove`]). Anything that cannot be
/// removed that way is left where it is and named.
///
/// Not supported → [`Blocker::NoExclusiveRename`] for `target`, nothing
/// left behind; a leftover of the trial → [`RelocateError::Cleanup`]
/// naming it. Never falls back to a rename that could replace an entry.
#[cfg(unix)]
fn probe_exclusive_rename(target: &Path, target_dir: &fs::File) -> Result<(), RelocateError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    const FROM: &std::ffi::CStr = c"rename-probe";
    const TO: &std::ffi::CStr = c"rename-probe-moved";
    // Not the run's `race` hook: the trial is not the cleanup it watches.
    let aside = Aside::create(target, target_dir, &mut no_race).map_err(copy_err)?;
    let file = match at::open_at(
        &aside.dir,
        FROM,
        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        0o600,
    ) {
        Ok(file) => file,
        Err(e) => {
            let made = format!(
                "{} could not be created: {e}",
                aside.describe(std::ffi::OsStr::new("rename-probe"))
            );
            return Err(match aside.remove(target_dir, Ok(())) {
                Ok(()) => copy_err(made),
                Err(left) => RelocateError::Cleanup {
                    error: Box::new(copy_err(made)),
                    left: vec![left],
                },
            });
        }
    };
    let renamed = at::rename_no_replace(&aside.dir, FROM, &aside.dir, TO);
    let name = if renamed.is_ok() { TO } else { FROM };
    let shown = || aside.describe(std::ffi::OsStr::from_bytes(name.to_bytes()));
    let removed = match (at::lstat(&aside.dir, name), file.metadata()) {
        (Ok(st), Ok(m))
            if at::kind(&st) == libc::S_IFREG && at::identity(&st) == (m.dev(), m.ino()) =>
        {
            at::unlink(&aside.dir, name, false)
                .map_err(|e| format!("{} could not be removed: {e}", shown()))
        }
        _ => Err(format!(
            "{} was left in place: it is not the trial file this run made",
            shown()
        )),
    };
    let left = aside.remove(target_dir, removed).err();
    let error = match renamed {
        Ok(()) => None,
        Err(e) => Some(blocked(Blocker::NoExclusiveRename {
            path: target.to_path_buf(),
            reason: e.to_string(),
        })),
    };
    match (error, left) {
        (None, None) => Ok(()),
        (Some(error), None) => Err(error),
        (error, Some(left)) => Err(RelocateError::Cleanup {
            error: Box::new(error.unwrap_or_else(|| {
                copy_err(format!(
                    "the trial rename in {} worked, but its private folder could not be \
                     removed",
                    target.display()
                ))
            })),
            left: vec![left],
        }),
    }
}

/// Windows renames through handles ([`anchored::move_child`]) and does not
/// copy at all ([`crate::namespace`]); other systems have no exclusive
/// rename, which [`at::rename_no_replace`] already refuses.
#[cfg(not(unix))]
fn probe_exclusive_rename(_: &Path, _: &fs::File) -> Result<(), RelocateError> {
    Ok(())
}

/// The target folder, opened once before anything is created in it. Staging
/// is accepted only inside it, publication moves entries into it through
/// the handle, and the bootstrap names the target only while its path
/// still leads here.
#[derive(Debug)]
struct TargetDir {
    handle: same_file::Handle,
}

impl TargetDir {
    /// Open `path` (a folder the user chose; links in it are followed, as
    /// everywhere else).
    /// The folder open as `file` (the admitted target, [`Protected::dir`]).
    fn from_file(file: fs::File) -> io::Result<Self> {
        Ok(Self {
            handle: same_file::Handle::from_file(file)?,
        })
    }

    fn file(&self) -> &fs::File {
        self.handle.as_file()
    }

    /// Whether the entry `name` in this folder, as itself, is `handle`.
    fn holds(&self, name: &str, handle: &same_file::Handle) -> bool {
        anchored::identity(self.file(), name)
            .and_then(same_file::Handle::from_file)
            .is_ok_and(|entry| entry == *handle)
    }

    /// `path` still leads to this folder; otherwise an error that says where
    /// the folder is now (if the system can say).
    fn still_at(&self, path: &Path) -> Result<(), String> {
        if same_file::Handle::from_path(path).is_ok_and(|now| now == self.handle) {
            return Ok(());
        }
        Err(self.displaced(path))
    }

    fn displaced(&self, path: &Path) -> String {
        match anchored::current_path(self.file()) {
            Some(now) => pc_core::tf!(
                "папку {0} переименовал или подменил кто-то другой; открытая для переноса \
                 папка теперь {1}; то, что сейчас лежит по пути {0}, не тронуто",
                "the folder {0} was renamed or replaced by someone else; the folder opened \
                 for the move is now {1}; whatever is at {0} now was not touched",
                path.display(),
                now.display()
            ),
            None => pc_core::tf!(
                "папку {0} переименовал или подменил кто-то другой; то, что сейчас лежит \
                 по пути {0}, не тронуто",
                "the folder {0} was renamed or replaced by someone else; whatever is at \
                 {0} now was not touched",
                path.display()
            ),
        }
    }
}

/// Make the copy at `target` the data folder, keeping the current one as
/// `previous` until the next launch proves the new one starts.
impl Copied {
    /// Commit while both writer locks are still held. Dropping an uncommitted
    /// copy leaves the bootstrap unchanged and keeps the verified copy.
    ///
    /// Order matters (review el-59w6z, B3). Everything that can wait on
    /// something outside — reading the current bootstrap, writing and
    /// flushing the new one — happens first. Then, with nothing left to
    /// wait for, the proof ([`Copied::prove_all`]): the protected paths
    /// still lead to the folders proven, `photo-cleanup.db` and `thumbs/` in
    /// the target are the proven objects (through the open folder and by
    /// path) and the database still carries this copy's generation. Only
    /// then is the new bootstrap renamed over the old one. If anything
    /// fails, the bootstrap is exactly as it was.
    ///
    /// A copy whose staging could not be removed is not committed: the
    /// error names the leftover. It says the copy is complete and verified
    /// only after the proof above has just passed; otherwise the error is
    /// [`RelocateError::Displaced`], which says where this run's own objects
    /// are now (or that their place is unknown) and that whatever is at the
    /// target's names is not the verified copy.
    pub fn commit(mut self) -> Result<(), RelocateError> {
        let current = current_choice(&self.dirs, self.source)?;
        if let Err(e) = self.prove() {
            return Err(match (e, self.staging_left.take()) {
                (RelocateError::Displaced { target, reason }, Some(left)) => {
                    RelocateError::Displaced {
                        target,
                        reason: format!("{reason}; {left}"),
                    }
                }
                (e, _) => e,
            });
        }
        if let Some(left) = self.staging_left.take() {
            return Err(RelocateError::Incomplete {
                copy: self.layout.dir.clone(),
                left,
            });
        }
        self.sidecars.ensure_owned().map_err(copy_err)?;
        self.spaces
            .settings
            .create_missing()
            .map_err(|r| blocked(unprotected(FolderRole::Settings, r)))?;
        // `layout.dir` is the proven, canonical path. The system choice is
        // recorded only if that is literally the system folder's path;
        // otherwise the canonical path itself, so that the start-up check
        // walks exactly the chain that was proven.
        let next = if self.layout.dir == self.dirs.system_data_dir() {
            Choice::system()
        } else {
            Choice::custom(self.layout.dir.clone())
        }
        .bound(self.binding()?);
        let bootstrap = self.spaces.settings.path().join(crate::BOOTSTRAP_FILE);
        write_bootstrap_checked(&bootstrap, &Bootstrap::new(next, Some(current)), || {
            self.prove_all()
        })
    }

    /// [`Copied::prove`], and the source and settings folders are still the
    /// ones admitted.
    fn prove_all(&self) -> Result<(), RelocateError> {
        self.prove()?;
        self.spaces.recheck()
    }

    /// The target's path still leads to the open target folder through the
    /// proven chain, and its `photo-cleanup.db` and `thumbs` are the proven
    /// objects — as entries of the open folder and at their paths — with
    /// the database still carrying this copy's generation.
    fn prove(&self) -> Result<(), RelocateError> {
        let displaced = |reason: String| RelocateError::Displaced {
            target: self.layout.dir.clone(),
            reason,
        };
        // The folder check first: it says where the folder is now.
        self.target_dir
            .still_at(&self.layout.dir)
            .map_err(displaced)?;
        self.target_ns
            .recheck()
            .map_err(|r| displaced(r.to_string()))?;
        for (name, path, held, directory) in [
            (DB_FILE, &self.layout.db, &self.payload.db, false),
            (THUMBS_DIR, &self.layout.thumbs, &self.payload.thumbs, true),
        ] {
            if !(self.target_dir.holds(name, held) && same_entry(path, held, directory)) {
                return Err(displaced(lost(path, held)));
            }
        }
        let generation = binding::read_generation(self.payload.db.as_file())
            .ok()
            .flatten();
        if generation.as_deref() != Some(self.payload.generation.as_str()) {
            return Err(displaced(pc_core::tf!(
                "у проверенной базы {0} больше нет метки этой копии",
                "the verified database {0} no longer carries this copy's generation",
                self.layout.db.display()
            )));
        }
        Ok(())
    }

    /// What the bootstrap records about the proven objects.
    fn binding(&self) -> Result<Binding, RelocateError> {
        let volume = namespace::volume_id(self.target_dir.file()).map_err(copy_err)?;
        let inode = |f: &fs::File| binding::inode(f).map_err(copy_err);
        Ok(Binding {
            volume,
            dir: inode(self.target_dir.file())?,
            db: inode(self.payload.db.as_file())?,
            thumbs: inode(self.payload.thumbs.as_file())?,
            generation: self.payload.generation.clone(),
        })
    }
}

/// `path` no longer is the proven object behind `held`: where that object
/// is now, as far as the system can say.
fn lost(path: &Path, held: &same_file::Handle) -> String {
    match anchored::current_path(held.as_file()) {
        Some(now) => pc_core::tf!(
            "проверенная копия {0} больше не лежит под этим именем: её переместил кто-то \
             другой, теперь она {1}; то, что сейчас по пути {0}, не тронуто и не является \
             проверенной копией",
            "the verified {0} is no longer at that name: someone else moved it, it is now \
             {1}; whatever is at {0} now was not touched and is not the verified copy",
            path.display(),
            now.display()
        ),
        None => pc_core::tf!(
            "проверенная копия {0} больше не лежит под этим именем, и где она теперь — \
             неизвестно; то, что сейчас по пути {0}, не тронуто и не является проверенной \
             копией",
            "the verified {0} is no longer at that name and where it is now is unknown; \
             whatever is at {0} now was not touched and is not the verified copy",
            path.display()
        ),
    }
}

/// Attempt to start the replacement process after a committed move. If it
/// cannot be spawned, restore the previous bootstrap before the caller
/// recovers its old server. The process launcher is injected for fault tests.
pub fn restart_or_restore(
    dirs: &SystemDirs,
    restart: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if let Err(error) = restart() {
        let rollback = read_bootstrap(&dirs.bootstrap_path()).and_then(|b| {
            let previous = b.and_then(|b| b.previous).ok_or(StartupError::NoPrevious)?;
            write_bootstrap(&dirs.bootstrap_path(), &Bootstrap::new(previous, None))
        });
        return Err(match rollback {
            Ok(()) => error,
            Err(rollback) => format!("{error}; cannot restore the previous bootstrap: {rollback}"),
        });
    }
    Ok(())
}

/// Use the database already in `target`, copying nothing. The current folder
/// becomes `previous`, exactly as after a move.
pub fn switch_to_existing(
    dirs: &SystemDirs,
    source: Source,
    target: &Path,
) -> Result<(), RelocateError> {
    if matches!(source, Source::Portable | Source::Override) {
        return Err(RelocateError::Blocked {
            blockers: vec![Blocker::ModeFixed { source }],
        });
    }
    let r = choose_data_dir(dirs, target, NewDir::UseExisting)?;
    let _target_lock =
        pc_core::lock::take_writer(&r.layout.db, "switching app data").map_err(|e| {
            RelocateError::Locked {
                reason: format!("{e:#}"),
            }
        })?;
    looks_like_ours(&r.layout.db).map_err(|reason| RelocateError::Verify { reason })?;
    if let Some(b) = &r.persist {
        write_bootstrap(&dirs.bootstrap_path(), b)?;
    }
    Ok(())
}

/// The choice now in effect, as the bootstrap should remember it.
fn current_choice(dirs: &SystemDirs, source: Source) -> Result<Choice, RelocateError> {
    match source {
        Source::Portable | Source::Override => Err(RelocateError::Blocked {
            blockers: vec![Blocker::ModeFixed { source }],
        }),
        Source::System | Source::Custom => Ok(read_bootstrap(&dirs.bootstrap_path())?
            .map(|b| b.current)
            .unwrap_or_else(Choice::system)),
    }
}

/// The database copy, proven, as the object that was proven.
struct Snapshot {
    db: same_file::Handle,
    generation: String,
    tables: usize,
    rows: u64,
}

/// Snapshot the source into a file this run owns, and verify that file.
///
/// 1. `photo-cleanup.db` is created by this run, exclusively (`O_EXCL`,
///    no link followed), relative to the open staging folder: an entry
///    already there — a foreign empty file included — fails the move and is
///    not touched (review el-59w6z, B1).
/// 2. SQLite takes a path, so `VACUUM INTO` may run only while the path
///    leads to that very file: the entry in the open staging folder and the
///    object at the path must both be it. `VACUUM INTO` fills an existing
///    file only if it is empty, and this one is ours and empty.
/// 3. After the copy the same check again, a flush, and this copy's
///    generation written onto the file through its descriptor.
/// 4. The verification reads that object, not a name: on macOS SQLite
///    opens `/dev/fd/N` of the held descriptor, which is that open file
///    whatever happens to names meanwhile (`immutable`: read-only, no
///    locks, no journal). Elsewhere SQLite opens the path, which the
///    protected namespace keeps leading to the file, and the check of
///    step 2 runs again afterwards.
///
/// A check that fails leaves whatever is at the names alone, publishes
/// nothing, and says where this run's own file is now.
fn snapshot(
    src: &Path,
    partial: &Path,
    staging: &OwnedDirectory,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<Snapshot, RelocateError> {
    let copy = partial.join(DB_FILE);
    let staging_dir = staging.handle.as_file();
    let own = anchored::create_file(staging_dir, DB_FILE)
        .and_then(same_file::Handle::from_file)
        .map_err(|e| {
            copy_err(pc_core::tf!(
                "файл копии базы {0} не создан этим переносом: {1}; ничего не изменено",
                "the database copy {0} could not be created by this run: {1}; nothing was \
                 changed",
                copy.display(),
                e
            ))
        })?;
    let here = |when: &str| -> Result<(), RelocateError> {
        let entry = anchored::identity(staging_dir, DB_FILE)
            .and_then(same_file::Handle::from_file)
            .is_ok_and(|entry| entry == own);
        if entry && same_entry(&copy, &own, false) {
            return Ok(());
        }
        let now = match anchored::current_path(own.as_file()) {
            Some(now) => now.display().to_string(),
            None => pc_core::tr!("неизвестно где", "at an unknown place").to_string(),
        };
        Err(copy_err(pc_core::tf!(
            "путь {0} ({1}) больше не ведёт к файлу копии, созданному этим переносом: папку \
             {2} или файл переименовал или подменил кто-то другой. Ничего не опубликовано; \
             собственный файл этого переноса — {3}; то, что лежит по пути {0}, не тронуто",
            "the path {0} ({1}) no longer leads to the database copy this run created: \
             someone else renamed or replaced {2} or the file. Nothing was published; this \
             run's own file is {3}; whatever is at {0} was not touched",
            copy.display(),
            when,
            partial.display(),
            now
        )))
    };
    here(pc_core::tr!("перед копированием", "before copying"))?;
    let conn = Connection::open_with_flags(
        src,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(copy_err)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(copy_err)?;
    let target = copy.to_str().ok_or_else(|| RelocateError::Copy {
        reason: format!("the path is not valid Unicode: {}", copy.display()),
    })?;
    conn.execute("VACUUM INTO ?1", [target]).map_err(copy_err)?;
    race(Step::Snapshotted, &copy);
    here(pc_core::tr!("после копирования", "after copying"))?;
    own.as_file().sync_all().map_err(copy_err)?;
    let generation = binding::new_generation().map_err(copy_err)?;
    binding::set_generation(own.as_file(), &generation)
        .and_then(|()| own.as_file().sync_all())
        .map_err(|e| {
            copy_err(pc_core::tf!(
                "метку копии не записать на {0}: {1}",
                "the copy's generation cannot be written onto {0}: {1}",
                copy.display(),
                e
            ))
        })?;
    race(Step::BeforeVerify, &copy);
    let proven = verify_owned(&conn, &copy, own.as_file())
        .map_err(|reason| RelocateError::Verify { reason })?;
    here(pc_core::tr!("после проверки", "after verification"))?;
    Ok(Snapshot {
        db: own,
        generation,
        tables: proven.0,
        rows: proven.1,
    })
}

/// [`verify`] reading the object open as `own` ([`snapshot`], step 4).
fn verify_owned(src: &Connection, copy: &Path, own: &fs::File) -> Result<(usize, u64), String> {
    #[cfg(target_os = "macos")]
    let dst = {
        use std::os::fd::AsRawFd;
        let _ = copy;
        Connection::open_with_flags(
            format!("file:/dev/fd/{}?mode=ro&immutable=1", own.as_raw_fd()),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| e.to_string())?
    };
    #[cfg(not(target_os = "macos"))]
    let dst = {
        let _ = own;
        Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())?
    };
    verify_connections(src, &dst)
}

/// The copy opens, is intact, and holds what the source holds.
pub fn verify(src: &Connection, copy: &Path) -> Result<(usize, u64), String> {
    let dst = Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    verify_connections(src, &dst)
}

/// Integrity, schema version, the list of tables and every table's row
/// count of `dst`, against `src`.
fn verify_connections(src: &Connection, dst: &Connection) -> Result<(usize, u64), String> {
    let integrity: String = dst
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if integrity != "ok" {
        return Err(format!("integrity_check: {integrity}"));
    }
    let schema = |c: &Connection| -> Result<Option<i64>, String> {
        c.query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .map_err(|e| e.to_string())
    };
    let (a, b) = (schema(src)?, schema(dst)?);
    if a != b {
        return Err(format!("schema version {b:?}, expected {a:?}"));
    }
    let tables = |c: &Connection| -> Result<Vec<String>, String> {
        let mut st = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(|e| e.to_string())?;
        let names = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string());
        names
    };
    let names = tables(src)?;
    if names != tables(dst)? {
        return Err("the list of tables differs".into());
    }
    let mut rows = 0u64;
    for name in &names {
        // Names come from sqlite_master, quoted as identifiers.
        let q = format!("SELECT count(*) FROM \"{}\"", name.replace('"', "\"\""));
        let count = |c: &Connection| -> Result<i64, String> {
            c.query_row(&q, [], |r| r.get(0)).map_err(|e| e.to_string())
        };
        let (a, b) = (count(src)?, count(dst)?);
        if a != b {
            return Err(format!("table {name}: {b} rows, expected {a}"));
        }
        rows += a.max(0) as u64;
    }
    Ok((names.len(), rows))
}

/// A database this app wrote: it opens, and it has our schema table.
fn looks_like_ours(db: &Path) -> Result<(), String> {
    let c = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    c.query_row("SELECT MAX(version) FROM schema_version", [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .map_err(|e| {
        pc_core::tf!(
            "это не база photo-cleanup: {0}",
            "this is not a photo-cleanup database: {0}",
            e
        )
    })?;
    Ok(())
}

/// Removes what this run put in the target, unless the run succeeded.
///
/// [`Cleanup::abort`] reports what it could not remove; `Drop` is only the
/// last resort (a panic) and has nowhere to report to.
struct Cleanup {
    /// The admitted target; folders it created are removed on abort.
    target: Option<Protected>,
    sidecars: Option<SidecarReservations>,
    /// The private staging folder (`photo-cleanup.db.partial`), with the
    /// database copy and `thumbs/` inside until they are published.
    db: Option<OwnedDirectory>,
    /// The thumbnails once published as `thumbs/`, until the database is.
    thumbs: Option<OwnedDirectory>,
}

impl Cleanup {
    fn new(target: Protected) -> Self {
        Self {
            target: Some(target),
            sidecars: None,
            db: None,
            thumbs: None,
        }
    }

    /// The copy is published: keep it, the thumbnails and the reservations;
    /// remove the database's now-empty staging folder. A failure there is
    /// returned, not fatal — the copy itself is complete — and
    /// [`Copied::commit`] turns it into an error.
    fn succeed_with(&mut self, race: &mut dyn FnMut(Step, &Path)) -> Option<String> {
        if let Some(thumbs) = &mut self.thumbs {
            thumbs.keep = true;
        }
        self.thumbs = None;
        self.db.take().and_then(|db| db.release_with(race).err())
    }

    /// Undo this run, one line per thing it could not remove.
    fn abort(&mut self) -> Vec<String> {
        let mut left = Vec::new();
        for owned in [self.thumbs.take(), self.db.take()].into_iter().flatten() {
            left.extend(owned.release().err());
        }
        if let Some(sidecars) = self.sidecars.take() {
            left.extend(sidecars.release());
        }
        if let Some(target) = &mut self.target {
            // `rmdir` removes only an empty folder: one that someone put
            // something into meanwhile (our own writer lock included) stays,
            // and that is not an error.
            let _ = target.remove_created();
        }
        left
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.abort();
    }
}

/// A lookup must notice dangling links and fail closed on unreadable entries.
fn occupied(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

fn legacy_partials(target: &Path) -> Vec<PathBuf> {
    let db = target.join(PARTIAL_DB);
    let mut paths = vec![db.clone(), target.join(PARTIAL_THUMBS)];
    paths.extend(SIDECARS.map(|suffix| sidecar(&db, suffix)));
    paths
}

/// The old staging names next to ours, checked again after the preview: the
/// SQLite sidecars of the old `.partial` file, and `thumbs.partial` (no
/// longer used, since the thumbnails are staged inside
/// `photo-cleanup.db.partial/`, but a leftover or a link appearing there is
/// still a reason to stop, as before).
fn reject_legacy_sidecars(target: &Path) -> Result<(), RelocateError> {
    let blockers: Vec<_> = SIDECARS
        .iter()
        .map(|suffix| sidecar(&target.join(PARTIAL_DB), suffix))
        .chain([target.join(PARTIAL_THUMBS)])
        .filter(|path| occupied(path))
        .map(|path| Blocker::LeftoverPartial { path })
        .collect();
    if blockers.is_empty() {
        Ok(())
    } else {
        Err(RelocateError::Blocked { blockers })
    }
}

/// The handle stays open, so an unlinked file's identity cannot be recycled.
/// Never follow a replacement symlink, including a link back to our inode.
fn same_entry(path: &Path, handle: &same_file::Handle, directory: bool) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        !m.file_type().is_symlink()
            && if directory { m.is_dir() } else { m.is_file() }
            && same_file::Handle::from_path(path).is_ok_and(|current| current == *handle)
    })
}

/// What kind of entry a cleanup owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owned {
    /// A staging directory with everything in it.
    Directory,
    /// A reservation that must still be empty.
    EmptyFile,
}

impl Owned {
    fn is_ours(self, path: &Path, handle: &same_file::Handle) -> bool {
        match self {
            Self::Directory => same_entry(path, handle, true),
            Self::EmptyFile => {
                same_entry(path, handle, false)
                    && handle.as_file().metadata().is_ok_and(|m| m.len() == 0)
            }
        }
    }

    /// [`Owned::is_ours`] for `name` in the open directory `dir`: the entry
    /// itself (never a link's target) is the object behind `handle`.
    #[cfg(unix)]
    fn is_ours_at(self, dir: &fs::File, name: &std::ffi::CStr, handle: &same_file::Handle) -> bool {
        let wanted = match self {
            Self::Directory => libc::S_IFDIR,
            Self::EmptyFile => libc::S_IFREG,
        };
        at::lstat(dir, name).is_ok_and(|st| {
            at::kind(&st) == wanted
                && at::identity(&st) == (handle.dev(), handle.ino())
                && (self == Self::Directory
                    || handle.as_file().metadata().is_ok_and(|m| m.len() == 0))
        })
    }
}

/// Points inside [`remove_owned_with`] where another process could act.
/// Tests act there; production passes [`no_race`]. Windows deletes through
/// handles and has only `Created` and `BeforeDelete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(unix), allow(dead_code))]
enum Step {
    /// `OwnedDirectory::create_with` made the directory; its identity is
    /// about to be taken by name.
    Created,
    /// The staging folder is registered and proven to be in the open
    /// target folder; the database is about to be copied into it.
    Registered,
    /// SQLite has written the snapshot into this run's own file; its path
    /// is about to be checked again.
    Snapshotted,
    /// The snapshot is checked, flushed and carries its generation; it is
    /// about to be verified.
    BeforeVerify,
    /// Database and thumbnails are copied and proven in the staging folder;
    /// the target's path is about to be checked.
    Verified,
    /// The target's path was checked; the proven copies are about to be
    /// moved out of the staging folder into the target.
    Publish,
    /// The public name was checked; it is about to be moved aside.
    BeforeMove,
    /// A private cleanup folder was made; it is about to be checked.
    AsideCreated,
    /// Moved aside, not yet checked there.
    Moved,
    /// Proven ours in the private folder (Windows: at its name); about to
    /// be deleted.
    BeforeDelete,
}

fn no_race(_: Step, _: &Path) {}

/// Remove `path` only if it is still the object behind `handle`.
///
/// POSIX has no "unlink this name only if it is still that inode", so a
/// check of `path` followed by a delete of `path` can delete whatever
/// another process renamed onto `path` in between. And every *name* the
/// cleanup uses — `path`, its parent, a private folder next to it — can be
/// renamed by anyone allowed to rename entries in the folder that holds it
/// (review el-5null: the whole private folder was renamed after the proof
/// and a foreign tree put under the old names; deleting by path deleted
/// it). So on Unix, after the parent is opened, nothing is addressed by a
/// path again; every step names an entry relative to an open directory
/// descriptor (`*at` system calls):
///
/// 1. The parent is opened once. A replaced, changed or symlinked entry at
///    `name` in it is left alone.
/// 2. A fresh folder with a unique name is created beside it, private to
///    this user from the start, opened relative to the parent descriptor
///    and checked through its own descriptor ([`Aside::create`]); `name`
///    is renamed into that descriptor without replacement. A rename moves
///    exactly the entry that is at `name` at that instant — never follows
///    a symlink, never replaces anything.
/// 3. The moved entry is checked again in the private folder, through its
///    descriptor. If it is not ours (it was swapped in after step 1) it is
///    renamed back into the parent descriptor without replacement; if the
///    name was taken again meanwhile it stays in the private folder and
///    the error says where. Nothing is deleted.
/// 4. Only an entry proven ours in the private folder is deleted, relative
///    to the private folder's descriptor, a directory entry by entry
///    through descriptors ([`at::remove_tree`]). Renaming the private
///    folder, the parent or any ancestor meanwhile changes nothing: the
///    descriptors still point at the folders that were checked, and no
///    other account can change what is inside the private folder.
/// 5. The private folder is removed by name only if that name still holds
///    the folder behind its descriptor; otherwise the name is left alone
///    and the error says where the private folder is now (if the system
///    can say: `F_GETPATH` on macOS, `/proc/self/fd` on Linux).
///
/// Windows needs none of this: an object can be deleted through a handle
/// to it. The name is checked, and then the object behind `handle` is
/// deleted wherever it is by then, a directory entry by entry through
/// handles opened by file ID (`windows_acl::delete_tree`). No path is used
/// for the delete.
///
/// What this proves, and under which conditions (DESKTOP.md, "Очистка
/// временных файлов"): if `handle` really is the object this run created,
/// no other entry is deleted — whatever is renamed onto `path`, onto the
/// private folder's name or onto any ancestor, before or after any check.
/// That needs both preconditions:
///
/// - `handle` was captured correctly. `OwnedDirectory::create_with` opens
///   it by name after creating it and accepts it only as an empty directory
///   of this user that no other account can use, opened without following
///   links ([`open_created_dir`]); a swap by another OS user fails that
///   check and nothing is registered (el-2xri).
/// - The private folder and our own staging directory are not manipulated
///   with the user's own authority. Owner-only access (Unix mode 0700
///   without extended ACL entries, a protected owner-only DACL on Windows)
///   keeps other OS users out of them; it is **not** a boundary against a
///   process running as the same user, root or admin, which can change the
///   user's files directly anyway and is outside the product's threat
///   model.
///
/// Known limits, pinned by tests: (a) on Unix a replacement is briefly
/// absent from `path` between steps 2 and 3; (b) whatever is moved into
/// the private folder, or into our own staging directory, after the step 3
/// check is treated as ours; (c) bytes written through a descriptor opened
/// on our own reservation (possible through its public name before step 2)
/// go with it; (d) step 5 is a check and then `rmdir` of the name: an
/// *empty* folder renamed onto the private folder's name between the two
/// is removed (`rmdir` never removes contents). Every failure returns an
/// error naming what was left and where — the original name or the private
/// folder.
fn remove_owned(path: &Path, handle: &same_file::Handle, kind: Owned) -> Result<(), String> {
    remove_owned_with(path, handle, kind, &mut no_race)
}

#[cfg(unix)]
fn remove_owned_with(
    path: &Path,
    handle: &same_file::Handle,
    kind: Owned,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let kept = |why: &dyn fmt::Display| format!("{} was left in place: {why}", path.display());
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(kept(&"it has no parent folder"));
    };
    let entry = std::ffi::CString::new(name.as_bytes()).map_err(|e| kept(&e))?;
    let parent_dir = match at::open_dir(parent) {
        // Gone with its parent: nothing of ours is at this name.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return not_at_name(path, handle),
        Err(e) => return Err(kept(&e)),
        Ok(dir) => dir,
    };
    match at::lstat(&parent_dir, &entry) {
        // Gone already (SQLite consumes empty sidecars): nothing to do.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return not_at_name(path, handle),
        Err(e) => return Err(kept(&e)),
        Ok(_) if !kind.is_ours_at(&parent_dir, &entry, handle) => {
            return Err(kept(&"it was replaced or changed by someone else"))
        }
        Ok(_) => {}
    }
    let aside = Aside::create(parent, &parent_dir, race).map_err(|e| kept(&e))?;
    race(Step::BeforeMove, path);
    if let Err(e) = at::rename_no_replace(&parent_dir, &entry, &aside.dir, &entry) {
        let result = match e.kind() {
            io::ErrorKind::NotFound => not_at_name(path, handle),
            _ => Err(kept(&e)),
        };
        return aside.remove(&parent_dir, result);
    }
    race(Step::Moved, &aside.path.join(name));
    if !kind.is_ours_at(&aside.dir, &entry, handle) {
        return match at::rename_no_replace(&aside.dir, &entry, &parent_dir, &entry) {
            Ok(()) => aside.remove(&parent_dir, Err(kept(&"it was replaced by someone else"))),
            Err(e) => Err(format!(
                "{} replaced {} and was moved aside; it could not be put back ({e}). Nothing was deleted",
                aside.describe(name),
                path.display()
            )),
        };
    }
    race(Step::BeforeDelete, &aside.path.join(name));
    match kind {
        Owned::Directory => at::remove_tree(&aside.dir, &entry),
        Owned::EmptyFile => at::unlink(&aside.dir, &entry, false),
    }
    .map_err(|e| {
        format!(
            "{} (this run's own {}) could not be removed: {e}",
            aside.describe(name),
            path.display()
        )
    })?;
    aside.remove(&parent_dir, Ok(()))
}

#[cfg(windows)]
fn remove_owned_with(
    path: &Path,
    handle: &same_file::Handle,
    kind: Owned,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<(), String> {
    let kept = |why: &dyn fmt::Display| format!("{} was left in place: {why}", path.display());
    match fs::symlink_metadata(path) {
        // Gone already (SQLite consumes empty sidecars): nothing to do.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return not_at_name(path, handle),
        Err(e) => return Err(kept(&e)),
        Ok(_) if !kind.is_ours(path, handle) => {
            return Err(kept(&"it was replaced or changed by someone else"))
        }
        Ok(_) => {}
    }
    race(Step::BeforeDelete, path);
    let own = |why: &dyn fmt::Display| {
        format!(
            "this run's own {} could not be removed: {why}; whatever is at that name now \
             was not touched",
            path.display()
        )
    };
    if kind == Owned::EmptyFile && !handle.as_file().metadata().is_ok_and(|m| m.len() == 0) {
        return Err(kept(&"it was written to by someone else"));
    }
    windows_acl::delete_tree(handle.as_file(), kind == Owned::Directory).map_err(|e| own(&e))
}

#[cfg(not(any(unix, windows)))]
fn remove_owned_with(
    path: &Path,
    _: &same_file::Handle,
    _: Owned,
    _: &mut dyn FnMut(Step, &Path),
) -> Result<(), String> {
    Err(format!(
        "{} was left in place: no safe way to remove it on this platform",
        path.display()
    ))
}

/// Nothing is at `path` any more. That is done only if the object behind
/// `handle` is gone too (deleted — SQLite consumes empty sidecars — or gone
/// with its parent). If it still exists it was moved away with its name or
/// a folder above it (review el-2ztq8: the target renamed after the check):
/// it is left where it is, and the error says where, if the system can say.
fn not_at_name(path: &Path, handle: &same_file::Handle) -> Result<(), String> {
    #[cfg(unix)]
    let exists = {
        use std::os::unix::fs::MetadataExt;
        handle.as_file().metadata().is_ok_and(|m| m.nlink() > 0)
    };
    // A deleted (or delete-pending) object may still report a path, e.g.
    // under `$Extend\$Deleted`: only one that can be looked up counts.
    #[cfg(not(unix))]
    let exists = anchored::current_path(handle.as_file()).is_some_and(|now| now.exists());
    if !exists {
        return Ok(());
    }
    Err(match anchored::current_path(handle.as_file()) {
        Some(now) => format!(
            "{} is no longer at that name: this run's own entry was moved by someone else \
             to {} and was left there",
            path.display(),
            now.display()
        ),
        None => format!(
            "{} is no longer at that name: this run's own entry was moved by someone else \
             to an unknown place and was left there",
            path.display()
        ),
    })
}

/// Prefix of the private folders cleanup moves entries into (Unix).
#[cfg(any(unix, test))]
const ASIDE_PREFIX: &str = ".photo-cleanup-removing";

/// A new, empty folder next to an entry being removed, private to this
/// user, held open: every operation in it goes through [`Aside::dir`].
#[cfg(unix)]
struct Aside {
    /// Where it was made; only for messages and test hooks.
    path: PathBuf,
    name: std::ffi::CString,
    dir: fs::File,
}

#[cfg(unix)]
impl Aside {
    /// A folder in `parent` that nobody else has a name for, made private
    /// to this user at creation ([`create_private_dir`]), then opened
    /// relative to `parent_dir` without following links and checked
    /// through that descriptor like a staging directory
    /// ([`fresh_private_dir`]) before anything is moved into it. A folder
    /// that fails the check is left where it is, and the error names it.
    fn create(
        parent: &Path,
        parent_dir: &fs::File,
        race: &mut dyn FnMut(Step, &Path),
    ) -> io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        for _ in 0..16 {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let name = format!("{ASIDE_PREFIX}-{}-{nanos:x}-{n}", std::process::id());
            let path = parent.join(&name);
            match create_private_dir(&path) {
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!(
                            "the private folder {} could not be created: {e}",
                            path.display()
                        ),
                    ))
                }
                Ok(()) => {}
            }
            race(Step::AsideCreated, &path);
            let name = std::ffi::CString::new(name)?;
            let opened = at::open_dir_at(parent_dir, &name).and_then(|dir| {
                // SAFETY: `geteuid` has no preconditions and cannot fail.
                let euid = unsafe { libc::geteuid() };
                fresh_private_dir(&dir, euid)
                    .map_err(|why| io::Error::new(io::ErrorKind::AlreadyExists, why))?;
                Ok(dir)
            });
            return match opened {
                Ok(dir) => Ok(Self { path, name, dir }),
                Err(e) => Err(io::Error::new(
                    e.kind(),
                    format!(
                        "the private folder {} cannot be proven to be the one just made ({e}); \
                         it was left in place",
                        path.display()
                    ),
                )),
            };
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free name for a private cleanup folder",
        ))
    }

    /// `name` in this folder, for a message: where it was made, and where
    /// the folder is now if someone moved it.
    fn describe(&self, name: &std::ffi::OsStr) -> String {
        let made = self.path.join(name);
        match at::current_path(&self.dir) {
            Some(now) if !same_place(&now, &self.path) => format!(
                "{} (its private folder was moved by someone else to {})",
                made.display(),
                now.display()
            ),
            _ => made.display().to_string(),
        }
    }

    /// Remove the (by now empty) folder and add a failure to do so to
    /// `result`: an empty `.photo-cleanup-removing-*` folder left behind is
    /// a leftover too, and is reported like one. The name is removed only
    /// while it still holds this folder; `rmdir` never removes contents.
    fn remove(self, parent_dir: &fs::File, result: Result<(), String>) -> Result<(), String> {
        let ours = match (at::lstat(parent_dir, &self.name), self.dir.metadata()) {
            (Ok(st), Ok(m)) => {
                use std::os::unix::fs::MetadataExt;
                at::identity(&st) == (m.dev(), m.ino())
            }
            _ => false,
        };
        let removed = if ours {
            at::unlink(parent_dir, &self.name, true)
                .map_err(|e| format!("{} could not be removed: {e}", self.path.display()))
        } else {
            Err(format!(
                "the private folder {} was moved or replaced by someone else; whatever is at \
                 that name now was left in place{}",
                self.path.display(),
                match at::current_path(&self.dir) {
                    Some(now) => format!(", and the private folder is now {}", now.display()),
                    None => String::new(),
                }
            ))
        };
        match (removed, result) {
            (Ok(()), result) => result,
            (Err(e), Ok(())) => Err(e),
            (Err(e), Err(why)) => Err(format!("{why}; {e}")),
        }
    }
}

/// Descriptor-relative file operations for [`remove_owned_with`]: once a
/// directory is open, entries in it are named relative to its descriptor,
/// so renaming that directory or any of its ancestors cannot redirect them
/// (review el-5null).
#[cfg(unix)]
mod at {
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::{fs, io};

    fn check(rc: libc::c_int) -> io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// Open the directory at `path` (a path the user chose; links in it are
    /// followed as everywhere else).
    pub(super) fn open_dir(path: &Path) -> io::Result<fs::File> {
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)
    }

    /// Open the directory `name` in `dir`; a symlink or a file there fails.
    pub(super) fn open_dir_at(dir: &fs::File, name: &CStr) -> io::Result<fs::File> {
        // SAFETY: `dir` is an open descriptor, `name` NUL-terminated; a
        // returned descriptor is new and owned by the `File` below.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nobody else owns.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    /// `lstat` of `name` in `dir`.
    pub(super) fn lstat(dir: &fs::File, name: &CStr) -> io::Result<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `st` is writable; on success it is fully initialized.
        check(unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        // SAFETY: `fstatat` succeeded.
        Ok(unsafe { st.assume_init() })
    }

    /// Device and inode, as `std` and `same_file` report them.
    #[allow(clippy::unnecessary_cast)] // the field types differ between systems
    pub(super) fn identity(st: &libc::stat) -> (u64, u64) {
        (st.st_dev as u64, st.st_ino as u64)
    }

    /// The type bits of `st`: `libc::S_IFDIR`, `libc::S_IFREG`, ...
    pub(super) fn kind(st: &libc::stat) -> libc::mode_t {
        st.st_mode & libc::S_IFMT
    }

    /// Rename `from` in `from_dir` to `to` in `to_dir`, failing if `to`
    /// exists ([`pc_core::disk::rename_no_replace_at`]). Other Unix systems
    /// have no such call: refuse.
    pub(super) fn rename_no_replace(
        from_dir: &fs::File,
        from: &CStr,
        to_dir: &fs::File,
        to: &CStr,
    ) -> io::Result<()> {
        #[cfg(test)]
        if fault::refuses(from, to) {
            // What `renameatx_np(RENAME_EXCL)` returns on exFAT (el-21zyg).
            return Err(io::Error::from_raw_os_error(libc::ENOTSUP));
        }
        // The one implementation, shared with pc-apply's moves (el-usdqi).
        pc_core::disk::rename_no_replace_at(from_dir, from, to_dir, to)
    }

    /// `unlinkat`: a file, or (`directory`) an empty directory.
    pub(super) fn unlink(dir: &fs::File, name: &CStr, directory: bool) -> io::Result<()> {
        let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: an open descriptor and a NUL-terminated name.
        check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) })
    }

    /// Delete the directory `name` in `dir` and everything in it. Each
    /// level is opened relative to the one above without following links;
    /// a link inside is removed, never followed.
    pub(super) fn remove_tree(dir: &fs::File, name: &CStr) -> io::Result<()> {
        let sub = open_dir_at(dir, name)?;
        for child in names(&sub)? {
            if kind(&lstat(&sub, &child)?) == libc::S_IFDIR {
                remove_tree(&sub, &child)?;
            } else {
                unlink(&sub, &child, false)?;
            }
        }
        drop(sub);
        unlink(dir, name, true)
    }

    /// `mkdirat`: a new directory `name` in `dir`; an existing entry fails.
    pub(super) fn mkdir(dir: &fs::File, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
        // SAFETY: an open descriptor and a NUL-terminated name.
        check(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode) })
    }

    /// `openat` of `name` in `dir` with `flags` (plus `O_NOFOLLOW` and
    /// `O_CLOEXEC`): a symlink at `name` fails, never followed.
    pub(super) fn open_at(
        dir: &fs::File,
        name: &CStr,
        flags: libc::c_int,
        mode: libc::mode_t,
    ) -> io::Result<fs::File> {
        // SAFETY: an open descriptor and a NUL-terminated name; a returned
        // descriptor is new and owned by the `File` below.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                libc::c_uint::from(mode),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor nobody else owns.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    /// Copy the regular files and folders under `from` into the directory
    /// open as `to`, creating every entry exclusively relative to an open
    /// descriptor: an existing entry fails the copy and keeps its bytes, and
    /// renaming `to` or any folder above it changes nothing about where the
    /// copy goes. Anything but a file or a folder in `from` stops the copy.
    pub(super) fn copy_tree(from: &Path, to: &fs::File) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let name = CString::new(entry.file_name().as_bytes())?;
            if kind.is_dir() {
                mkdir(to, &name, 0o777)?;
                copy_tree(&entry.path(), &open_dir_at(to, &name)?)?;
            } else if kind.is_file() {
                let mut out = open_at(
                    to,
                    &name,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    0o666,
                )?;
                io::copy(&mut fs::File::open(entry.path())?, &mut out)?;
                out.sync_all()?;
            } else {
                return Err(super::unexpected(&entry.path()));
            }
        }
        to.sync_all()
    }

    /// Files and bytes under the directory open as `dir`, listed and
    /// examined through descriptors. Anything but a file or a folder is an
    /// error.
    pub(super) fn tree_size(dir: &fs::File) -> io::Result<(u64, u64)> {
        let (mut files, mut bytes) = (0u64, 0u64);
        for name in names(dir)? {
            let st = lstat(dir, &name)?;
            match kind(&st) {
                libc::S_IFDIR => {
                    let (f, b) = tree_size(&open_dir_at(dir, &name)?)?;
                    files += f;
                    bytes = bytes.saturating_add(b);
                }
                libc::S_IFREG => {
                    files += 1;
                    #[allow(clippy::unnecessary_cast)] // `off_t` differs between systems
                    let len = st.st_size.max(0) as u64;
                    bytes = bytes.saturating_add(len);
                }
                _ => {
                    return Err(io::Error::other(format!(
                        "not a regular file or folder: {}",
                        name.to_string_lossy()
                    )))
                }
            }
        }
        Ok((files, bytes))
    }

    /// Where the directory open as `dir` is now, if the system can say.
    pub(super) fn current_path(dir: &fs::File) -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::ffi::OsStrExt;
            let mut buf = vec![0u8; libc::PATH_MAX as usize];
            // SAFETY: `F_GETPATH` writes at most `PATH_MAX` bytes to `buf`.
            if unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
                return None;
            }
            let path = CStr::from_bytes_until_nul(&buf).ok()?;
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
        }
        #[cfg(target_os = "linux")]
        return fs::read_link(format!("/proc/self/fd/{}", dir.as_raw_fd())).ok();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = dir;
            None
        }
    }

    /// The names in the directory open as `dir`, without `.`/`..`. Lists
    /// the descriptor, not a path. A read error is an error.
    pub(super) fn names(dir: &fs::File) -> io::Result<Vec<CString>> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        fn errno() -> *mut libc::c_int {
            // SAFETY: returns this thread's errno location; always valid.
            unsafe { libc::__errno_location() }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        fn errno() -> *mut libc::c_int {
            // SAFETY: returns this thread's errno location; always valid.
            unsafe { libc::__error() }
        }

        // `fdopendir` takes ownership of the descriptor it is given: give it
        // a duplicate, so `dir` (and the identity behind it) stays open.
        // SAFETY: `dir` owns a valid descriptor for the duration of the call.
        let fd = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor we own; on success the stream
        // owns it and `closedir` below closes it, on failure we close it.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: `fd` is still ours when `fdopendir` failed.
            unsafe { libc::close(fd) };
            return Err(e);
        }
        // A duplicate shares the offset of `dir`: start from the beginning.
        // SAFETY: `stream` is a valid stream until `closedir`.
        unsafe { libc::rewinddir(stream) };
        let mut out = Vec::new();
        let result = loop {
            // SAFETY: `errno()` points at this thread's errno; `stream` is
            // valid.
            unsafe { *errno() = 0 };
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                // SAFETY: as above.
                let code = unsafe { *errno() };
                break if code == 0 {
                    Ok(out)
                } else {
                    Err(io::Error::from_raw_os_error(code))
                };
            }
            // SAFETY: a non-null `readdir` result points at a valid entry
            // with a NUL-terminated name until the next `readdir`/`closedir`.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if name.to_bytes() != b"." && name.to_bytes() != b".." {
                out.push(name.to_owned());
            }
        };
        // SAFETY: `stream` is valid and closed exactly once.
        unsafe { libc::closedir(stream) };
        result
    }

    /// Tests only: make [`rename_no_replace`] on this thread fail the way
    /// exFAT does (`ENOTSUP`), for the calls a rule picks, without a volume
    /// that both passes the admission and lacks the call — macOS has none.
    #[cfg(test)]
    pub(super) mod fault {
        use std::cell::RefCell;
        use std::ffi::CStr;

        type Rule = Box<dyn FnMut(&CStr, &CStr) -> bool>;

        thread_local! {
            static RULE: RefCell<Option<Rule>> = const { RefCell::new(None) };
        }

        /// While the guard lives, a rename from `from` to `to` fails when
        /// `rule(from, to)` says so.
        pub(in super::super) fn refuse(rule: impl FnMut(&CStr, &CStr) -> bool + 'static) -> Guard {
            RULE.with(|r| *r.borrow_mut() = Some(Box::new(rule)));
            Guard
        }

        pub(in super::super) struct Guard;

        impl Drop for Guard {
            fn drop(&mut self) {
                RULE.with(|r| *r.borrow_mut() = None);
            }
        }

        pub(super) fn refuses(from: &CStr, to: &CStr) -> bool {
            RULE.with(|r| r.borrow_mut().as_mut().is_some_and(|rule| rule(from, to)))
        }
    }
}

/// The few operations that publication needs, each relative to an open
/// directory handle and never through a name in the shared target
/// (review el-2ztq8).
mod anchored {
    use std::path::PathBuf;
    use std::{fs, io};

    /// The entry `name` in the directory open as `dir`, opened as itself
    /// (a symlink there fails or is opened as the link). Enough to compare
    /// identities and for [`move_child`] to check what it moves.
    #[cfg(unix)]
    pub(super) fn child(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        let name = std::ffi::CString::new(name)?;
        super::at::open_at(dir, &name, libc::O_RDONLY | libc::O_NONBLOCK, 0)
    }

    /// Windows has no `child`: copying is refused there before anything is
    /// written ([`crate::namespace`]), so nothing is published from a
    /// staging folder.
    #[cfg(not(any(unix, windows)))]
    pub(super) fn child(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        let _ = (dir, name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no safe way to publish on this platform",
        ))
    }

    /// Create the regular file `name` in the directory open as `dir`,
    /// exclusively (an existing entry of any kind fails and is not
    /// touched), readable and writable by this user only.
    #[cfg(unix)]
    pub(super) fn create_file(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        let name = std::ffi::CString::new(name)?;
        super::at::open_at(
            dir,
            &name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
    }

    #[cfg(not(unix))]
    pub(super) fn create_file(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        let _ = (dir, name);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no safe way to create the copy on this platform",
        ))
    }

    /// Only the identity of `name` in `dir`: on Windows opened without
    /// `DELETE` access, so it never conflicts with another handle's sharing.
    #[cfg(windows)]
    pub(super) fn identity(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        super::windows_acl::child(dir, name.as_ref(), false)
    }

    #[cfg(not(windows))]
    pub(super) fn identity(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        child(dir, name)
    }

    /// Move `name` out of the directory open as `from` into the directory
    /// open as `to`, under the same name, never replacing anything there.
    /// `entry` is what [`child`] opened at `name` in `from`: the move is
    /// refused unless `name` still is that entry (Unix; Windows renames the
    /// object behind `entry` itself), and afterwards `name` in `to` must be
    /// it. `from` is this run's private staging folder, into which no other
    /// account can put anything, so the object moved is ours whatever
    /// happened to the names in the shared target meanwhile.
    #[cfg(unix)]
    pub(super) fn move_child(
        from: &fs::File,
        name: &str,
        entry: &same_file::Handle,
        to: &fs::File,
    ) -> io::Result<()> {
        use super::at;
        let c = std::ffi::CString::new(name)?;
        let is_entry = |dir: &fs::File| {
            at::lstat(dir, &c).is_ok_and(|st| at::identity(&st) == (entry.dev(), entry.ino()))
        };
        if !is_entry(from) {
            return Err(io::Error::other(format!(
                "{name} in the staging folder is not the proven copy"
            )));
        }
        at::rename_no_replace(from, &c, to, &c)?;
        if !is_entry(to) {
            return Err(io::Error::other(format!(
                "{name} in the target is not the proven copy after the move"
            )));
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn move_child(
        _from: &fs::File,
        name: &str,
        entry: &same_file::Handle,
        to: &fs::File,
    ) -> io::Result<()> {
        super::windows_acl::rename_into(entry.as_file(), to, name.as_ref())
    }

    #[cfg(not(any(unix, windows)))]
    pub(super) fn move_child(
        _: &fs::File,
        _: &str,
        _: &same_file::Handle,
        _: &fs::File,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no safe way to publish on this platform",
        ))
    }

    /// Where the object open as `file` is now, if the system can say.
    pub(super) fn current_path(file: &fs::File) -> Option<PathBuf> {
        #[cfg(unix)]
        return super::at::current_path(file);
        #[cfg(windows)]
        return super::windows_acl::final_path(file);
        #[cfg(not(any(unix, windows)))]
        {
            let _ = file;
            None
        }
    }
}

struct OwnedDirectory {
    path: PathBuf,
    handle: same_file::Handle,
    keep: bool,
}

impl OwnedDirectory {
    #[cfg(test)]
    fn create(path: &Path) -> io::Result<Self> {
        Self::create_with(path, &mut no_race)
    }

    /// Create `path` as a new directory and register it as this run's own.
    ///
    /// `mkdir` returns no descriptor, so the identity can only be taken by
    /// opening the name afterwards, and something else may be at the name
    /// by then (el-2xri: a foreign folder with photos in it was registered
    /// and later deleted as ours). The opened entry is therefore accepted
    /// only if it could be the directory just made: see
    /// [`open_created_dir`]. Otherwise nothing is registered and nothing
    /// is removed — neither what is at `path` now nor, wherever it went,
    /// the directory this call made — and the error says so.
    ///
    /// The directory is made private to this user by the creating call
    /// itself, not afterwards: [`create_private_dir`].
    fn create_with(path: &Path, race: &mut dyn FnMut(Step, &Path)) -> io::Result<Self> {
        create_private_dir(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{} could not be created as a private folder: {e}",
                    path.display()
                ),
            )
        })?;
        race(Step::Created, path);
        let file = open_created_dir(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "{} was created by this run, but the entry now at that name \
                     cannot be proven to be it ({e}); nothing was removed, \
                     whatever is there was left in place",
                    path.display()
                ),
            )
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            handle: same_file::Handle::from_file(file)?,
            keep: false,
        })
    }

    fn release(self) -> Result<(), String> {
        self.release_with(&mut no_race)
    }

    /// Remove the folder if it is still ours at its name. If it is not, the
    /// error also says where this run's own folder is now (if the system can
    /// say), so every uncertain entry is named with its path.
    fn release_with(mut self, race: &mut dyn FnMut(Step, &Path)) -> Result<(), String> {
        self.keep = true;
        remove_owned_with(&self.path, &self.handle, Owned::Directory, race).map_err(|why| {
            match anchored::current_path(self.handle.as_file()) {
                Some(now)
                    if !same_place(&now, &self.path)
                        && !why.contains(&now.display().to_string()) =>
                {
                    format!(
                        "{why}; this run's own folder {} was moved by someone else to {} and \
                     was left there",
                        self.path.display(),
                        now.display()
                    )
                }
                _ => why,
            }
        })
    }
}

impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = remove_owned(&self.path, &self.handle, Owned::Directory);
        }
    }
}

/// Create `path` as a new directory that only this user can use, with that
/// access set by the creating call itself — never loosened first and
/// tightened afterwards. An existing entry at `path` fails the call
/// (`AlreadyExists`) and is not touched.
///
/// - Unix: `mkdir` with mode 0700 (umask can only take bits away). On
///   Linux a default ACL inherited from the parent is limited by that mode:
///   the ACL mask becomes the group bits, i.e. none.
/// - macOS: additionally, extended ACL entries are inherited from the
///   parent regardless of the mode (`everyone allow add_file,delete_child
///   ... directory_inherit` would open a 0700 folder to everybody; review
///   el-1wh7b). `mkdirx_np` creates the folder with mode 0700 and an empty
///   ACL flagged `ACL_FLAG_NO_INHERIT`, so nothing is inherited.
/// - Windows: `CreateDirectoryW` with a security descriptor whose owner is
///   this user and whose protected DACL (no inheritance from the parent)
///   has one entry: full control for this user, inherited by the contents.
///
/// Whatever the platform did, [`open_created_dir`] checks the result.
fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return macos_acl::mkdir_private(path);
    #[cfg(windows)]
    return windows_acl::mkdir_private(path);
    #[cfg(all(unix, not(target_os = "macos")))]
    return {
        let mut builder = fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(path)
    };
    #[cfg(not(any(unix, windows)))]
    return fs::create_dir(path);
}

/// Open the entry at `path` that [`create_private_dir`] has just created,
/// and refuse it unless it could be that directory.
///
/// Unix: opened with `O_NOFOLLOW | O_DIRECTORY`, so a symlink or a file put
/// on the name fails to open; then, through the descriptor (not the name
/// again), it must be a directory owned by this process's effective user,
/// without group/other permission bits, without extended ACL entries
/// (macOS) and empty ([`fresh_private_dir`]). A directory swapped in by
/// another OS user fails the owner check — nobody but root can create one
/// owned by us. A swap by a process running as the same user is outside the
/// threat model (DESKTOP.md, «Очистка временных файлов»): an empty private
/// directory it made is indistinguishable from ours, and then becomes the
/// one this run fills and later removes.
///
/// Windows: opened without following a reparse point; through the handle it
/// must be a directory that is not a reparse point, owned by this user,
/// with a protected DACL that allows access to nobody but this user, and
/// empty (`windows_acl::check`). Another account cannot create a folder
/// owned by us without restore/take-ownership privileges (administrator
/// level, outside the model); a folder made with default, inherited
/// permissions fails the DACL check even when it is ours.
#[cfg(unix)]
fn open_created_dir(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    fresh_private_dir(&file, euid)
        .map_err(|why| io::Error::new(io::ErrorKind::AlreadyExists, why))?;
    Ok(file)
}

#[cfg(windows)]
fn open_created_dir(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    const GENERIC_READ: u32 = 0x8000_0000;
    const DELETE: u32 = 0x0001_0000;
    // `DELETE`: release deletes this very object through the handle
    // (`windows_acl::delete_tree`), never whatever is at the name then.
    let file = fs::OpenOptions::new()
        .access_mode(GENERIC_READ | DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let meta = file.metadata()?;
    let checked = if !meta.is_dir() || meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Err("it is not a plain directory".to_string())
    } else {
        volume::check_open(&file).and_then(|()| windows_acl::check(&file))
    };
    checked.map_err(|why| io::Error::new(io::ErrorKind::AlreadyExists, why))?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_created_dir(path: &Path) -> io::Result<fs::File> {
    let _ = path;
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no safe way to register a new directory on this platform",
    ))
}

/// Why the directory open as `file` cannot be the one
/// [`create_private_dir`] just made for `euid`. Checked through the
/// descriptor only.
#[cfg(unix)]
fn fresh_private_dir(file: &fs::File, euid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("it is not a directory".into());
    }
    if meta.uid() != euid {
        return Err(format!(
            "it belongs to user {}, not to this user ({euid})",
            meta.uid()
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "its permissions are {:o}, not private to this user",
            meta.mode() & 0o7777
        ));
    }
    // On a volume that ignores owners every user sees our uid on it.
    volume::check_open(file)?;
    #[cfg(target_os = "macos")]
    macos_acl::no_extended_acl(file)?;
    if !dir_is_empty(file).map_err(|e| format!("its contents cannot be listed: {e}"))? {
        return Err("it is not empty".into());
    }
    Ok(())
}

/// macOS extended ACLs (`chmod +a`), which apply on top of — and
/// regardless of — the mode bits.
#[cfg(target_os = "macos")]
mod macos_acl {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::{fs, io};

    // <sys/acl.h>, <sys/fcntl.h>
    const ACL_TYPE_EXTENDED: c_int = 0x100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_FLAG_NO_INHERIT: c_int = 1 << 17;
    const FILESEC_MODE: c_int = 4;
    const FILESEC_ACL: c_int = 5;

    extern "C" {
        fn acl_init(count: c_int) -> *mut c_void;
        fn acl_free(obj: *mut c_void) -> c_int;
        fn acl_get_flagset_np(obj: *mut c_void, flagset: *mut *mut c_void) -> c_int;
        fn acl_add_flag_np(flagset: *mut c_void, flag: c_int) -> c_int;
        fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn filesec_init() -> *mut c_void;
        fn filesec_free(fsec: *mut c_void);
        fn filesec_set_property(fsec: *mut c_void, property: c_int, value: *const c_void) -> c_int;
        fn mkdirx_np(path: *const c_char, fsec: *mut c_void) -> c_int;
    }

    /// `mkdir` with mode 0700 and an empty, non-inheriting ACL.
    pub(super) fn mkdir_private(path: &Path) -> io::Result<()> {
        let name = CString::new(path.as_os_str().as_bytes())?;
        let mode: libc::mode_t = 0o700;
        // SAFETY: every object is created, used and freed here; the
        // pointers passed are valid for the duration of each call, and
        // `filesec_set_property` copies what it is given.
        unsafe {
            let acl = acl_init(0);
            if acl.is_null() {
                return Err(io::Error::last_os_error());
            }
            let fsec = filesec_init();
            if fsec.is_null() {
                let e = io::Error::last_os_error();
                acl_free(acl);
                return Err(e);
            }
            let mut flags: *mut c_void = std::ptr::null_mut();
            let prepared = acl_get_flagset_np(acl, &mut flags) == 0
                && acl_add_flag_np(flags, ACL_FLAG_NO_INHERIT) == 0
                && filesec_set_property(fsec, FILESEC_ACL, (&raw const acl).cast()) == 0
                && filesec_set_property(fsec, FILESEC_MODE, (&raw const mode).cast()) == 0;
            let result = if !prepared {
                Err(io::Error::last_os_error())
            } else if mkdirx_np(name.as_ptr(), fsec) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            };
            filesec_free(fsec);
            acl_free(acl);
            result
        }
    }

    /// Refuse a directory that has any extended ACL entry. A filesystem
    /// without ACL support (`ENOTSUP`) cannot grant anything through one;
    /// such volumes are refused before this anyway ([`super::volume`]).
    pub(super) fn no_extended_acl(file: &fs::File) -> Result<(), String> {
        // SAFETY: `file` owns a valid descriptor; the returned ACL, if any,
        // is freed exactly once below.
        let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(libc::ENOENT | libc::ENOTSUP | libc::EOPNOTSUPP) => Ok(()),
                _ => Err(format!("its access list cannot be read: {e}")),
            };
        }
        let mut entry: *mut c_void = std::ptr::null_mut();
        // SAFETY: `acl` is a valid ACL until `acl_free`.
        let has_entry = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &mut entry) } == 0;
        // SAFETY: as above; freed once.
        unsafe { acl_free(acl) };
        if has_entry {
            Err("it has an access list (ACL) that may let other users in".into())
        } else {
            Ok(())
        }
    }
}

/// Windows security descriptors for the private folders: see
/// [`create_private_dir`] and [`open_created_dir`].
#[cfg(windows)]
mod windows_acl {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;
    use std::{fs, io, iter, mem, ptr};
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
        ERROR_NOT_SUPPORTED, ERROR_NO_MORE_FILES, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        AclSizeInformation, EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl,
        GetTokenInformation, TokenUser, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
        SECURITY_ATTRIBUTES, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateDirectoryW, FileDispositionInfo, FileDispositionInfoEx, FileFullDirectoryInfo,
        FileFullDirectoryRestartInfo, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
        FileIdType, GetFileInformationByHandleEx, OpenFileById, SetFileInformationByHandle, DELETE,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
        FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
        FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_FULL_DIR_INFO, FILE_ID_BOTH_DIR_INFO,
        FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;

    /// The `TOKEN_USER` of this process, in an 8-byte aligned buffer.
    pub(super) struct User(Vec<u64>);

    impl User {
        pub(super) fn current() -> io::Result<Self> {
            let mut token: HANDLE = ptr::null_mut();
            // SAFETY: the pseudo-handle of this process needs no closing;
            // `token` is closed below.
            if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut len = 0u32;
            // SAFETY: a size query with no buffer; fails with the size.
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut len) };
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            // SAFETY: `buf` holds at least `len` bytes.
            let ok = unsafe {
                GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len)
            };
            let e = io::Error::last_os_error();
            // SAFETY: `token` was opened above and is closed once.
            unsafe { CloseHandle(token) };
            if ok == 0 {
                return Err(e);
            }
            Ok(Self(buf))
        }

        pub(super) fn sid(&self) -> PSID {
            // SAFETY: the buffer was filled with a TOKEN_USER; its SID
            // points into the same buffer, which lives as long as `self`.
            unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
        }

        pub(super) fn sid_string(&self) -> io::Result<String> {
            let mut text: *mut u16 = ptr::null_mut();
            // SAFETY: `sid` is valid; `text` is freed with LocalFree.
            if unsafe { ConvertSidToStringSidW(self.sid(), &mut text) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `text` is a NUL-terminated wide string.
            let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
            let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
            // SAFETY: allocated by ConvertSidToStringSidW, freed once.
            unsafe { LocalFree(text.cast()) };
            Ok(sid)
        }
    }

    fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
        text.encode_wide().chain(iter::once(0)).collect()
    }

    /// A security descriptor from SDDL, freed on drop.
    pub(super) struct Descriptor(PSECURITY_DESCRIPTOR);

    impl Descriptor {
        pub(super) fn from_sddl(sddl: &str) -> io::Result<Self> {
            let text = wide(sddl.as_ref());
            let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
            // SAFETY: `text` is NUL-terminated; `sd` is freed on drop.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    text.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(sd))
        }

        /// Create `path` with this descriptor.
        pub(super) fn mkdir(&self, path: &Path) -> io::Result<()> {
            let attrs = SECURITY_ATTRIBUTES {
                nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.0,
                bInheritHandle: 0,
            };
            let name = wide(path.as_os_str());
            // SAFETY: `name` is NUL-terminated, `attrs` valid for the call.
            if unsafe { CreateDirectoryW(name.as_ptr(), &attrs) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }

    impl Drop for Descriptor {
        fn drop(&mut self) {
            // SAFETY: allocated by the SDDL conversion, freed once.
            unsafe { LocalFree(self.0) };
        }
    }

    /// SDDL: owner this user; protected DACL, full control for this user,
    /// inherited by files and folders inside.
    pub(super) fn private_sddl(user: &User) -> io::Result<String> {
        let sid = user.sid_string()?;
        Ok(format!("O:{sid}D:P(A;OICI;FA;;;{sid})"))
    }

    pub(super) fn mkdir_private(path: &Path) -> io::Result<()> {
        let user = User::current()?;
        Descriptor::from_sddl(&private_sddl(&user)?)?.mkdir(path)
    }

    /// Why the directory open as `file` cannot be one
    /// [`mkdir_private`] just made for this user.
    pub(super) fn check(file: &fs::File) -> Result<(), String> {
        let user = User::current().map_err(|e| format!("this user cannot be determined: {e}"))?;
        let handle = file.as_raw_handle() as HANDLE;
        let mut owner: PSID = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `handle` is open with READ_CONTROL (GENERIC_READ);
        // `owner`/`dacl` point into `sd`, freed below.
        let rc = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut sd,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(format!(
                "its permissions cannot be read: {}",
                io::Error::from_raw_os_error(rc as i32)
            ));
        }
        // SAFETY: `sd` and the pointers into it are valid until LocalFree.
        let result = unsafe { check_descriptor(&user, owner, dacl, sd) }.and_then(|()| {
            match dir_is_empty(handle) {
                Ok(true) => Ok(()),
                Ok(false) => Err("it is not empty".into()),
                Err(e) => Err(format!("its contents cannot be listed: {e}")),
            }
        });
        // SAFETY: allocated by GetSecurityInfo, freed once.
        unsafe { LocalFree(sd) };
        result
    }

    /// # Safety
    /// `owner`, `dacl` and `sd` come from one successful `GetSecurityInfo`
    /// and are still allocated.
    unsafe fn check_descriptor(
        user: &User,
        owner: PSID,
        dacl: *mut ACL,
        sd: PSECURITY_DESCRIPTOR,
    ) -> Result<(), String> {
        if owner.is_null() || EqualSid(owner, user.sid()) == 0 {
            return Err("it belongs to another account, not to this user".into());
        }
        let (mut control, mut revision) = (0u16, 0u32);
        if GetSecurityDescriptorControl(sd, &mut control, &mut revision) == 0 {
            return Err(format!(
                "its permissions cannot be read: {}",
                io::Error::last_os_error()
            ));
        }
        if control & SE_DACL_PROTECTED == 0 {
            return Err("its permissions are inherited from the parent folder".into());
        }
        if dacl.is_null() {
            return Err("it has no access list, so everybody has full access".into());
        }
        let mut info: ACL_SIZE_INFORMATION = mem::zeroed();
        if GetAclInformation(
            dacl,
            (&raw mut info).cast::<c_void>(),
            mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ) == 0
        {
            return Err(format!(
                "its access list cannot be read: {}",
                io::Error::last_os_error()
            ));
        }
        for i in 0..info.AceCount {
            let mut ace: *mut c_void = ptr::null_mut();
            if GetAce(dacl, i, &mut ace) == 0 {
                return Err(format!(
                    "its access list cannot be read: {}",
                    io::Error::last_os_error()
                ));
            }
            match (*ace.cast::<ACE_HEADER>()).AceType {
                // Denying something to somebody never lets anyone in.
                ACCESS_DENIED_ACE_TYPE => {}
                ACCESS_ALLOWED_ACE_TYPE => {
                    let sid = (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast();
                    if EqualSid(sid, user.sid()) == 0 {
                        return Err("its access list lets other accounts in".into());
                    }
                }
                _ => return Err("its access list has entries this check does not know".into()),
            }
        }
        Ok(())
    }

    /// The entry `name` in the directory open as `dir`, found by listing the
    /// handle and opened by file ID as itself (no reparse point followed).
    /// With `delete`, opened with `DELETE` and listing access, as
    /// [`delete_tree`] and [`rename_into`] need.
    pub(super) fn child(
        dir: &fs::File,
        name: &std::ffi::OsStr,
        delete: bool,
    ) -> io::Result<fs::File> {
        let handle = dir.as_raw_handle() as HANDLE;
        let wanted: Vec<u16> = name.encode_wide().collect();
        let id = children(handle)?
            .into_iter()
            .find(|(entry, _)| *entry == wanted)
            .map(|(_, id)| id)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} is not in the folder", name.to_string_lossy()),
                )
            })?;
        let access = if delete {
            DELETE | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE
        } else {
            FILE_READ_ATTRIBUTES | SYNCHRONIZE
        };
        open_by_id_with(handle, id, access)
    }

    /// Rename the object open as `entry` (with `DELETE` access) to `name`
    /// in the directory open as `to`, through the handles only
    /// (`FILE_RENAME_INFO` with `RootDirectory`): no path is resolved, and
    /// an existing entry at `name` fails the call and is kept.
    pub(super) fn rename_into(
        entry: &fs::File,
        to: &fs::File,
        name: &std::ffi::OsStr,
    ) -> io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            FileRenameInfo, FILE_RENAME_INFO, FILE_RENAME_INFO_0,
        };
        let wide: Vec<u16> = name.encode_wide().collect();
        let header = mem::offset_of!(FILE_RENAME_INFO, FileName);
        let bytes = header + wide.len() * 2 + 2;
        let mut buf = vec![0u64; bytes.div_ceil(8)];
        let info = buf.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        // SAFETY: `buf` is 8-byte aligned and holds the header plus the name
        // and a terminating NUL; the fields are written in place.
        unsafe {
            (*info).Anonymous = FILE_RENAME_INFO_0 { ReplaceIfExists: 0 };
            (*info).RootDirectory = to.as_raw_handle() as HANDLE;
            (*info).FileNameLength = (wide.len() * 2) as u32;
            ptr::copy_nonoverlapping(
                wide.as_ptr(),
                (&raw mut (*info).FileName).cast::<u16>(),
                wide.len(),
            );
        }
        // SAFETY: `entry` is open with DELETE; `buf` lives for the call.
        let ok = unsafe {
            SetFileInformationByHandle(
                entry.as_raw_handle() as HANDLE,
                FileRenameInfo,
                buf.as_ptr().cast(),
                bytes as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Where the object open as `file` is now.
    pub(super) fn final_path(file: &fs::File) -> Option<std::path::PathBuf> {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
        };
        let handle = file.as_raw_handle() as HANDLE;
        let mut buf = vec![0u16; 1024];
        loop {
            // SAFETY: `buf` is writable for its whole length.
            let n = unsafe {
                GetFinalPathNameByHandleW(
                    handle,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if n == 0 {
                return None;
            }
            if n < buf.len() {
                return Some(std::ffi::OsString::from_wide(&buf[..n]).into());
            }
            buf.resize(n + 1, 0);
        }
    }

    /// Delete the object open as `file` — a file, or (`directory`) a
    /// directory with everything in it — through handles only, wherever it
    /// is now: the entries inside are listed through the directory's own
    /// handle and opened by file ID, never by name, and each is deleted
    /// through its own handle (review el-5null). A junction or symlink
    /// inside is opened as itself and deleted, never followed. `file` must
    /// have been opened with `DELETE` access.
    pub(super) fn delete_tree(file: &fs::File, directory: bool) -> io::Result<()> {
        delete_handle(file.as_raw_handle() as HANDLE, directory)
    }

    fn delete_handle(handle: HANDLE, directory: bool) -> io::Result<()> {
        if directory {
            for id in child_ids(handle)? {
                let child = open_by_id(handle, id)?;
                let attributes = {
                    use std::os::windows::fs::MetadataExt;
                    child.metadata()?.file_attributes()
                };
                let is_dir = attributes & FILE_ATTRIBUTE_DIRECTORY != 0
                    && attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0;
                delete_handle(child.as_raw_handle() as HANDLE, is_dir)?;
            }
        }
        dispose(handle)
    }

    /// Open the entry with file ID `id` on the volume of `volume`, as
    /// itself (no reparse point is followed), for deleting.
    fn open_by_id(volume: HANDLE, id: i64) -> io::Result<fs::File> {
        open_by_id_with(
            volume,
            id,
            DELETE | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY | SYNCHRONIZE,
        )
    }

    fn open_by_id_with(volume: HANDLE, id: i64, access: u32) -> io::Result<fs::File> {
        use std::os::windows::io::FromRawHandle;
        let descriptor = FILE_ID_DESCRIPTOR {
            dwSize: mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id },
        };
        // SAFETY: `volume` is an open handle, `descriptor` lives for the
        // call; a returned handle is new and owned by the `File` below.
        let child = unsafe {
            OpenFileById(
                volume,
                &descriptor,
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            )
        };
        if child == INVALID_HANDLE_VALUE || child.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle nobody else owns.
        Ok(unsafe { fs::File::from_raw_handle(child.cast()) })
    }

    /// Delete the object behind `handle`: POSIX semantics (the name goes at
    /// once) where the file system has them, otherwise delete-on-close.
    fn dispose(handle: HANDLE) -> io::Result<()> {
        let info = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_FLAG_DELETE
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        };
        // SAFETY: `handle` is open with DELETE; `info` lives for the call.
        let ok = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfoEx,
                (&raw const info).cast(),
                mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        let unsupported = [
            ERROR_INVALID_PARAMETER,
            ERROR_NOT_SUPPORTED,
            ERROR_INVALID_FUNCTION,
        ]
        .map(|code| Some(code as i32));
        if !unsupported.contains(&e.raw_os_error()) {
            return Err(e);
        }
        let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
        // SAFETY: as above.
        let ok = unsafe {
            SetFileInformationByHandle(
                handle,
                FileDispositionInfo,
                (&raw const info).cast(),
                mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// File IDs of the entries in the directory behind `handle`, without
    /// `.`/`..`. Lists the handle, not a path.
    fn child_ids(handle: HANDLE) -> io::Result<Vec<i64>> {
        Ok(children(handle)?.into_iter().map(|(_, id)| id).collect())
    }

    /// Names and file IDs of the entries in the directory behind `handle`,
    /// without `.`/`..`. Lists the handle, not a path.
    fn children(handle: HANDLE) -> io::Result<Vec<(Vec<u16>, i64)>> {
        let mut buf = [0u64; 512];
        let mut class = FileIdBothDirectoryRestartInfo;
        let mut ids = Vec::new();
        loop {
            // SAFETY: `buf` is writable for its whole size.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    class,
                    buf.as_mut_ptr().cast(),
                    mem::size_of_val(&buf) as u32,
                )
            };
            if ok == 0 {
                let e = io::Error::last_os_error();
                return if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    Ok(ids)
                } else {
                    Err(e)
                };
            }
            class = FileIdBothDirectoryInfo;
            let mut offset = 0usize;
            loop {
                // SAFETY: the call filled `buf` with a chain of entries;
                // `NextEntryOffset` stays within it.
                let (name, id, next) = unsafe {
                    let entry = buf
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<FILE_ID_BOTH_DIR_INFO>();
                    let len = (*entry).FileNameLength as usize / 2;
                    let name = std::slice::from_raw_parts(
                        (&raw const (*entry).FileName).cast::<u16>(),
                        len,
                    );
                    (name, (*entry).FileId, (*entry).NextEntryOffset)
                };
                if name != [u16::from(b'.')] && name != [u16::from(b'.'), u16::from(b'.')] {
                    ids.push((name.to_vec(), id));
                }
                if next == 0 {
                    break;
                }
                offset += next as usize;
            }
        }
    }

    /// Whether the directory behind `handle` has no entries besides
    /// `.`/`..`. Lists the handle, not a path.
    fn dir_is_empty(handle: HANDLE) -> io::Result<bool> {
        let mut buf = [0u64; 512];
        let mut class = FileFullDirectoryRestartInfo;
        loop {
            // SAFETY: `buf` is writable for its whole size.
            let ok = unsafe {
                GetFileInformationByHandleEx(
                    handle,
                    class,
                    buf.as_mut_ptr().cast(),
                    mem::size_of_val(&buf) as u32,
                )
            };
            if ok == 0 {
                let e = io::Error::last_os_error();
                return if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    Ok(true)
                } else {
                    Err(e)
                };
            }
            class = FileFullDirectoryInfo;
            let mut offset = 0usize;
            loop {
                // SAFETY: the call filled `buf` with a chain of entries;
                // `NextEntryOffset` stays within it.
                let (name, next) = unsafe {
                    let entry = buf
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<FILE_FULL_DIR_INFO>();
                    let len = (*entry).FileNameLength as usize / 2;
                    let name = std::slice::from_raw_parts(
                        (&raw const (*entry).FileName).cast::<u16>(),
                        len,
                    );
                    (name, (*entry).NextEntryOffset)
                };
                if name != [u16::from(b'.')] && name != [u16::from(b'.'), u16::from(b'.')] {
                    return Ok(false);
                }
                if next == 0 {
                    break;
                }
                offset += next as usize;
            }
        }
    }
}

/// Whether the directory open as `file` has no entries besides `.`/`..`.
/// Lists the descriptor, not a path, so it is the same directory the
/// other checks looked at. A read error is an error, never "empty".
#[cfg(unix)]
fn dir_is_empty(file: &fs::File) -> io::Result<bool> {
    Ok(at::names(file)?.is_empty())
}

#[derive(Debug)]
struct EmptyReservation {
    path: PathBuf,
    handle: same_file::Handle,
}

impl EmptyReservation {
    fn claim(path: PathBuf) -> io::Result<Self> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        // Windows deletes a reservation through this handle: see
        // `remove_owned_with`.
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::access_mode(
            &mut options,
            0x8000_0000 | 0x4000_0000 | 0x0001_0000, // GENERIC_READ | GENERIC_WRITE | DELETE
        );
        let file = options.open(&path)?;
        Ok(Self {
            path,
            handle: same_file::Handle::from_file(file)?,
        })
    }

    fn is_ours(&self) -> bool {
        Owned::EmptyFile.is_ours(&self.path, &self.handle)
    }

    fn release_with(&self, race: &mut dyn FnMut(Step, &Path)) -> Result<(), String> {
        remove_owned_with(&self.path, &self.handle, Owned::EmptyFile, race)
    }
}

/// Zero-length WAL/SHM/journal files carry no transactions. They reserve the
/// SQLite namespace without a check-then-open race and belong to the new DB
/// after publication. SQLite may consume them on its first open; relocation
/// must not delete them on the way to that open, even across a process restart.
#[derive(Debug, Default)]
struct SidecarReservations {
    entries: Vec<EmptyReservation>,
    keep: bool,
}

impl SidecarReservations {
    /// Claim every sidecar name of `db`; those claimed before a failure are
    /// already recorded, so the caller's cleanup covers them.
    fn claim(&mut self, db: &Path) -> io::Result<()> {
        for suffix in SIDECARS {
            self.entries
                .push(EmptyReservation::claim(sidecar(db, suffix))?);
        }
        Ok(())
    }

    fn ensure_owned(&mut self) -> io::Result<()> {
        for entry in &mut self.entries {
            if !occupied(&entry.path) {
                // A read-only inspection of the published DB can make SQLite
                // remove empty sidecars. Reclaim exclusively before commit.
                *entry = EmptyReservation::claim(entry.path.clone())?;
            }
            if !entry.is_ours() {
                return Err(io::Error::other(format!(
                    "SQLite reservation changed: {}",
                    entry.path.display()
                )));
            }
        }
        Ok(())
    }

    /// Remove the reservations; one line per one that could not be.
    fn release(self) -> Vec<String> {
        self.release_with(&mut no_race)
    }

    fn release_with(mut self, race: &mut dyn FnMut(Step, &Path)) -> Vec<String> {
        self.keep = true;
        self.entries
            .iter()
            .filter_map(|entry| entry.release_with(race).err())
            .collect()
    }
}

impl Drop for SidecarReservations {
    fn drop(&mut self) {
        if !self.keep {
            for entry in &self.entries {
                let _ = entry.release_with(&mut no_race);
            }
        }
    }
}

fn copy_err(e: impl fmt::Display) -> RelocateError {
    RelocateError::Copy {
        reason: e.to_string(),
    }
}

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    db.with_file_name(name)
}

/// Files and bytes under `dir`; nothing if it does not exist.
fn tree_size(dir: &Path) -> io::Result<(u64, u64)> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    if !dir.exists() {
        return Ok((0, 0));
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                files += 1;
                bytes = bytes.saturating_add(entry.metadata()?.len());
            } else {
                return Err(unexpected(&entry.path()));
            }
        }
    }
    Ok((files, bytes))
}

/// Make `thumbs/` in the staging folder open as `staging`, copy the
/// thumbnail cache `from` into it (if there is one: an empty `thumbs/` is
/// made either way, so the binding always has a folder to record), and
/// count what arrived there.
///
/// Unix: `thumbs/` is made and filled relative to the staging folder's
/// descriptor ([`at::copy_tree`]) and counted through descriptors
/// ([`at::tree_size`]), so a staging name renamed or replaced in the shared
/// target meanwhile receives nothing and is not counted (review el-2ztq8).
/// The returned handle is the folder that was counted.
#[cfg(unix)]
fn stage_thumbs(from: &Path, staging: &fs::File) -> io::Result<(u64, u64, same_file::Handle)> {
    let name = std::ffi::CString::new(THUMBS_DIR)?;
    at::mkdir(staging, &name, 0o700)?;
    let dir = at::open_dir_at(staging, &name)?;
    if from.exists() {
        at::copy_tree(from, &dir)?;
    }
    let (files, bytes) = at::tree_size(&dir)?;
    Ok((files, bytes, same_file::Handle::from_file(dir)?))
}

/// Windows (review el-59w6z, B4): no descriptor-relative creation and
/// counting is implemented, and copying by path cannot be tied to the
/// staging folder. The move is refused before any write by the namespace
/// admission ([`crate::namespace`]); this is never reached, and refuses too.
#[cfg(not(unix))]
fn stage_thumbs(_: &Path, _: &fs::File) -> io::Result<(u64, u64, same_file::Handle)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the thumbnails cannot be copied safely on this platform",
    ))
}

fn unexpected(p: &Path) -> io::Error {
    io::Error::other(format!("not a regular file or folder: {}", p.display()))
}

fn sync_dir(dir: &fs::File) {
    #[cfg(unix)]
    let _ = dir.sync_all();
    #[cfg(not(unix))]
    let _ = dir;
}

/// The same folder, however it is spelled (`/var` vs `/private/var`).
fn same_place(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// `a` is `b` or somewhere below it. The nearest existing ancestor of `a` is
/// resolved, so a folder not yet created is still placed correctly.
fn under(a: &Path, b: &Path) -> bool {
    let Ok(b) = b.canonicalize() else {
        return a.starts_with(b);
    };
    let mut existing = a;
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(p), Some(n)) => {
                rest.push(n.to_os_string());
                existing = p;
            }
            _ => return a.starts_with(&b),
        }
    }
    let Ok(mut full) = existing.canonicalize() else {
        return a.starts_with(&b);
    };
    for n in rest.into_iter().rev() {
        full.push(n);
    }
    full.starts_with(&b)
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    #[test]
    fn cleanup_preserves_replaced_or_modified_reservations() {
        let temp = tempfile::tempdir().unwrap();
        for replace in [false, true] {
            for suffix in SIDECARS {
                let dir = temp.path().join(format!("{replace}{suffix}"));
                fs::create_dir(&dir).unwrap();
                let db = dir.join(DB_FILE);
                let mut reservations = SidecarReservations::default();
                reservations.claim(&db).unwrap();
                let foreign = sidecar(&db, suffix);
                let bytes: &[u8] = if replace {
                    // Even an empty foreign file is not ours to unlink.
                    fs::remove_file(&foreign).unwrap();
                    b""
                } else {
                    b"foreign write into the reservation"
                };
                fs::write(&foreign, bytes).unwrap();
                drop(reservations);
                assert_eq!(fs::read(&foreign).unwrap(), bytes);
                assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_preserves_symlinks_including_links_to_its_own_old_inode() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        for dangling in [false, true] {
            for suffix in SIDECARS {
                let dir = temp.path().join(format!("{dangling}{suffix}"));
                fs::create_dir(&dir).unwrap();
                let db = dir.join(DB_FILE);
                let mut reservations = SidecarReservations::default();
                reservations.claim(&db).unwrap();
                let link = sidecar(&db, suffix);
                let referent = dir.join("moved-reservation");
                if dangling {
                    fs::remove_file(&link).unwrap();
                } else {
                    fs::rename(&link, &referent).unwrap();
                }
                symlink(&referent, &link).unwrap();
                drop(reservations);
                assert_eq!(fs::read_link(link).unwrap(), referent);
                assert_eq!(referent.exists(), !dangling);
            }
        }
    }

    #[test]
    fn cleanup_does_not_remove_a_replacement_staging_directory() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        fs::rename(&path, temp.path().join("moved")).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("foreign"), b"keep").unwrap();
        drop(owned);
        assert_eq!(fs::read(path.join("foreign")).unwrap(), b"keep");
    }

    /// Every entry below `dir`: relative path → bytes, `<dir>`, or link target.
    fn tree(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut out = std::collections::BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap() {
                let e = e.unwrap();
                let rel = e.path().strip_prefix(dir).unwrap().display().to_string();
                if rel.starts_with(ASIDE_PREFIX) {
                    continue; // checked separately by `no_private_dirs_left`
                }
                let ty = e.file_type().unwrap();
                let v = if ty.is_symlink() {
                    format!("-> {}", fs::read_link(e.path()).unwrap().display()).into_bytes()
                } else if ty.is_dir() {
                    stack.push(e.path());
                    b"<dir>".to_vec()
                } else {
                    fs::read(e.path()).unwrap()
                };
                out.insert(rel, v);
            }
        }
        out
    }

    fn no_private_dirs_left(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.starts_with(ASIDE_PREFIX), "left {name}");
        }
    }

    /// A staging directory with some of the run's own content in it.
    fn staged(dir: &Path) -> (PathBuf, OwnedDirectory) {
        let path = dir.join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        fs::create_dir(path.join("sub")).unwrap();
        fs::write(path.join(DB_FILE), b"copy").unwrap();
        fs::write(path.join("sub/thumb.jpg"), b"thumb").unwrap();
        (path, owned)
    }

    /// Kinds of foreign entries a racing process may put on our name.
    #[derive(Clone, Copy, Debug)]
    enum Foreign {
        Dir,
        File,
        EmptyFile,
        #[cfg(unix)]
        Symlink,
    }

    const FOREIGN: &[Foreign] = &[
        Foreign::Dir,
        Foreign::File,
        Foreign::EmptyFile,
        #[cfg(unix)]
        Foreign::Symlink,
    ];

    /// Put `kind` at `path`; `outside` holds a symlink's payload.
    fn plant(kind: Foreign, path: &Path, outside: &Path) {
        match kind {
            Foreign::Dir => {
                fs::create_dir(path).unwrap();
                fs::write(path.join("photo.jpg"), b"\xff\xd8 foreign").unwrap();
            }
            Foreign::File => fs::write(path, b"foreign payload \x00\xff").unwrap(),
            Foreign::EmptyFile => fs::write(path, b"").unwrap(),
            #[cfg(unix)]
            Foreign::Symlink => {
                fs::create_dir_all(outside).unwrap();
                fs::write(outside.join("photo.jpg"), b"outside payload").unwrap();
                std::os::unix::fs::symlink(outside, path).unwrap();
            }
        }
        let _ = outside;
    }

    /// The reported regression (el-1e5d): the name is swapped after the
    /// identity check. Before the fix, `remove_dir_all`/`remove_file` of the
    /// name deleted the replacement (reproduced 0/2 on the old code).
    #[cfg(unix)]
    #[test]
    fn a_staging_directory_swapped_after_the_check_is_not_deleted() {
        for &kind in FOREIGN {
            let temp = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let (_, owned) = staged(temp.path());
            let ours = temp.path().join("ours-moved-away");
            let mut before = None;
            let result = owned.release_with(&mut |step, at| {
                if step == Step::BeforeMove {
                    fs::rename(at, &ours).unwrap();
                    plant(kind, at, &outside.path().join("o"));
                    before = Some((tree(temp.path()), tree(outside.path())));
                }
            });
            let error = result.expect_err("a replacement must be reported");
            assert!(error.contains("replaced"), "{kind:?}: {error}");
            let (inside, out) = before.unwrap();
            assert_eq!(tree(temp.path()), inside, "{kind:?}");
            assert_eq!(tree(outside.path()), out, "{kind:?}");
            no_private_dirs_left(temp.path());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_reservation_swapped_after_the_check_is_not_deleted() {
        for &kind in FOREIGN {
            for suffix in SIDECARS {
                let temp = tempfile::tempdir().unwrap();
                let outside = tempfile::tempdir().unwrap();
                let db = temp.path().join(DB_FILE);
                let mut reservations = SidecarReservations::default();
                reservations.claim(&db).unwrap();
                let target = sidecar(&db, suffix);
                let ours = temp.path().join("ours-moved-away");
                let mut before = None;
                let left = reservations.release_with(&mut |step, at| {
                    if step == Step::BeforeMove && at == target {
                        fs::rename(at, &ours).unwrap();
                        plant(kind, at, &outside.path().join("o"));
                        before = Some((tree(temp.path()), tree(outside.path())));
                    }
                });
                assert_eq!(left.len(), 1, "{kind:?}{suffix}: {left:?}");
                let (inside, out) = before.unwrap();
                // Our other two reservations went; the foreign entry stayed.
                let mut expected = inside;
                for other in SIDECARS.iter().filter(|s| **s != suffix) {
                    expected.remove(&format!("{DB_FILE}{other}"));
                }
                assert_eq!(tree(temp.path()), expected, "{kind:?}{suffix}");
                assert_eq!(tree(outside.path()), out, "{kind:?}{suffix}");
                no_private_dirs_left(temp.path());
            }
        }
    }

    /// Swapped in after the check, and the name taken again before it can be
    /// put back: both foreign entries survive, and the error says where.
    #[cfg(unix)]
    #[test]
    fn a_replacement_that_cannot_be_put_back_stays_aside_and_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let (path, owned) = staged(temp.path());
        let ours = temp.path().join("ours-moved-away");
        let mut aside = None;
        let error = owned
            .release_with(&mut |step, at| match step {
                Step::BeforeMove => {
                    fs::rename(at, &ours).unwrap();
                    plant(Foreign::Dir, at, at);
                }
                Step::Moved => {
                    aside = Some(at.to_path_buf());
                    fs::write(&path, b"second foreign").unwrap();
                }
                Step::BeforeDelete => panic!("nothing may be deleted"),
                Step::AsideCreated => {}
                Step::Created
                | Step::Registered
                | Step::Snapshotted
                | Step::BeforeVerify
                | Step::Verified
                | Step::Publish => {
                    unreachable!("release creates and publishes nothing")
                }
            })
            .unwrap_err();
        let aside = aside.unwrap();
        assert!(error.contains(&aside.display().to_string()), "{error}");
        assert!(error.contains("Nothing was deleted"), "{error}");
        assert_eq!(fs::read(&path).unwrap(), b"second foreign");
        assert_eq!(
            fs::read(aside.join("photo.jpg")).unwrap(),
            b"\xff\xd8 foreign"
        );
        assert_eq!(fs::read(ours.join(DB_FILE)).unwrap(), b"copy");
    }

    #[test]
    fn unchanged_entries_are_removed_without_leftovers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("someone-elses.txt"), b"keep").unwrap();
        let (_, owned) = staged(temp.path());
        let mut reservations = SidecarReservations::default();
        reservations.claim(&temp.path().join(DB_FILE)).unwrap();
        owned.release().unwrap();
        assert!(reservations.release().is_empty());
        assert_eq!(
            tree(temp.path()).into_keys().collect::<Vec<_>>(),
            ["someone-elses.txt"]
        );
    }

    #[test]
    fn a_reservation_consumed_meanwhile_is_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(DB_FILE);
        let mut reservations = SidecarReservations::default();
        reservations.claim(&db).unwrap();
        fs::remove_file(sidecar(&db, "-wal")).unwrap();
        // Gone between the check and the move, too.
        let left = reservations.release_with(&mut |step, at| {
            if step == Step::BeforeMove && at.ends_with(format!("{DB_FILE}-shm")) {
                fs::remove_file(at).unwrap();
            }
        });
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    /// Writes into our own reservation after it was claimed make it not ours.
    #[cfg(unix)]
    #[test]
    fn a_reservation_written_after_the_move_is_put_back() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(DB_FILE);
        let mut reservations = SidecarReservations::default();
        reservations.claim(&db).unwrap();
        let wal = sidecar(&db, "-wal");
        let left = reservations.release_with(&mut |step, at| {
            if step == Step::Moved && at.ends_with(format!("{DB_FILE}-wal")) {
                fs::write(at, b"late write").unwrap();
            }
        });
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(fs::read(&wal).unwrap(), b"late write");
        no_private_dirs_left(temp.path());
    }

    /// The documented limit: whatever someone moves into the private folder
    /// after the entry was proven ours there is treated as ours. This pins
    /// the contract (see `remove_owned`), it does not endorse it.
    #[cfg(unix)]
    #[test]
    fn the_private_folder_is_trusted_after_the_proof() {
        let temp = tempfile::tempdir().unwrap();
        let (_, owned) = staged(temp.path());
        let ours = temp.path().join("ours-moved-away");
        owned
            .release_with(&mut |step, at| {
                if step == Step::BeforeDelete {
                    fs::rename(at, &ours).unwrap();
                    fs::create_dir(at).unwrap();
                    fs::write(at.join("x"), b"inside the private folder").unwrap();
                }
            })
            .unwrap();
        assert_eq!(fs::read(ours.join(DB_FILE)).unwrap(), b"copy");
        no_private_dirs_left(temp.path());
    }

    /// Cleanup failures are errors that name the leftover, not a silent Ok.
    #[cfg(unix)]
    #[test]
    fn cleanup_errors_are_reported_and_nothing_else_is_touched() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("target");
        fs::create_dir(&parent).unwrap();
        let (path, owned) = staged(&parent);
        let mut reservations = SidecarReservations::default();
        reservations.claim(&parent.join(DB_FILE)).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(parent.join("probe"), b"").is_ok() {
            // Root ignores the mode; nothing to prove here.
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }
        let before = tree(&parent);
        let error = owned.release().unwrap_err();
        let left = reservations.release();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert_eq!(left.len(), 3, "{left:?}");
        assert_eq!(tree(&parent), before);
    }

    /// A delete that fails in the private folder is reported with its path.
    #[cfg(unix)]
    #[test]
    fn a_failed_delete_names_the_private_folder() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let (_, owned) = staged(temp.path());
        let mut locked = None;
        let result = owned.release_with(&mut |step, at| {
            if step == Step::BeforeDelete {
                let sub = at.join("sub");
                fs::set_permissions(&sub, fs::Permissions::from_mode(0o500)).unwrap();
                locked = Some(sub);
            }
        });
        let sub = locked.unwrap();
        let writable = fs::write(sub.join("probe"), b"").is_ok();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o700)).unwrap();
        if writable {
            return; // root
        }
        let error = result.unwrap_err();
        assert!(error.contains(ASIDE_PREFIX), "{error}");
        assert_eq!(fs::read(sub.join("thumb.jpg")).unwrap(), b"thumb");
    }

    /// Windows deletes the object behind the handle, not the name (review
    /// el-5null): an entry of any kind put on the name after the check is
    /// not touched, and our own staging is deleted where it was moved to.
    #[cfg(windows)]
    #[test]
    fn windows_deletes_our_object_not_what_is_at_the_name() {
        for &kind in FOREIGN {
            let temp = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let (_, owned) = staged(temp.path());
            let ours = temp.path().join("ours-moved-away");
            let mut before = None;
            owned
                .release_with(&mut |step, at| {
                    if step == Step::BeforeDelete {
                        fs::rename(at, &ours).unwrap();
                        plant(kind, at, &outside.path().join("o"));
                        before = Some((tree(temp.path()), tree(outside.path())));
                    }
                })
                .unwrap();
            let (mut inside, out) = before.unwrap();
            inside.retain(|k, _| !k.starts_with("ours-moved-away"));
            assert_eq!(tree(temp.path()), inside, "{kind:?}");
            assert_eq!(tree(outside.path()), out, "{kind:?}");
            assert!(!ours.exists(), "{kind:?}");
            no_private_dirs_left(temp.path());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_deletes_our_reservation_not_what_is_at_the_name() {
        for &kind in FOREIGN {
            let temp = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let db = temp.path().join(DB_FILE);
            let mut reservations = SidecarReservations::default();
            reservations.claim(&db).unwrap();
            let target = sidecar(&db, "-wal");
            let ours = temp.path().join("ours-moved-away");
            let mut before = None;
            let left = reservations.release_with(&mut |step, at| {
                if step == Step::BeforeDelete && at == target {
                    fs::rename(at, &ours).unwrap();
                    plant(kind, at, &outside.path().join("o"));
                    before = Some((tree(temp.path()), tree(outside.path())));
                }
            });
            assert!(left.is_empty(), "{kind:?}: {left:?}");
            let (mut inside, out) = before.unwrap();
            inside.retain(|k, _| {
                k != "ours-moved-away" && !k.ends_with("-shm") && !k.ends_with("-journal")
            });
            assert_eq!(tree(temp.path()), inside, "{kind:?}");
            assert_eq!(tree(outside.path()), out, "{kind:?}");
        }
    }

    /// Bytes written into our reservation after the name was checked keep
    /// it: the length is read again through the handle right before the
    /// delete.
    #[cfg(windows)]
    #[test]
    fn windows_keeps_a_reservation_written_after_the_check() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(DB_FILE);
        let mut reservations = SidecarReservations::default();
        reservations.claim(&db).unwrap();
        let wal = sidecar(&db, "-wal");
        let left = reservations.release_with(&mut |step, at| {
            if step == Step::BeforeDelete && at == wal {
                fs::write(at, b"late write").unwrap();
            }
        });
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].contains(&wal.display().to_string()), "{left:?}");
        assert_eq!(fs::read(&wal).unwrap(), b"late write");
    }

    /// The thumbnail copy never overwrites: an existing file at a
    /// destination name fails the copy and keeps its bytes. Unix only: the
    /// copy is refused on other systems before any thumbnail is written.
    #[cfg(unix)]
    #[test]
    fn the_thumbnail_copy_never_replaces_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        fs::create_dir_all(from.join("ab")).unwrap();
        fs::write(from.join("ab").join("abcd.jpg"), b"thumb").unwrap();
        fs::create_dir_all(to.join("ab")).unwrap();
        fs::write(to.join("ab").join("abcd.jpg"), b"foreign photo").unwrap();
        let copy_tree = |from: &Path, to: &Path| at::copy_tree(from, &at::open_dir(to).unwrap());
        let e = copy_tree(&from, &to).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(
            fs::read(to.join("ab").join("abcd.jpg")).unwrap(),
            b"foreign photo"
        );
        fs::remove_dir_all(to.join("ab")).unwrap();
        copy_tree(&from, &to).unwrap();
        assert_eq!(fs::read(to.join("ab").join("abcd.jpg")).unwrap(), b"thumb");
    }

    /// `copy_data`'s failure path returns what cleanup could not remove.
    #[test]
    fn abort_reports_a_replaced_staging_directory_and_keeps_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut cleanup = Cleanup::new(namespace::admit(temp.path()).unwrap());
        let (path, owned) = staged(temp.path());
        cleanup.db = Some(owned);
        fs::rename(&path, temp.path().join("moved")).unwrap();
        plant(Foreign::Dir, &path, &path);
        let left = cleanup.abort();
        assert_eq!(left.len(), 1, "{left:?}");
        assert_eq!(
            fs::read(path.join("photo.jpg")).unwrap(),
            b"\xff\xd8 foreign"
        );
        let error = RelocateError::Cleanup {
            error: Box::new(copy_err("disk full")),
            left,
        };
        let text = error.to_string();
        assert!(
            text.contains("disk full") && text.contains(PARTIAL_DB),
            "{text}"
        );
        assert!(cleanup.abort().is_empty());
    }
}

/// The desktop shell's copy → commit → restart decision goes through
/// [`move_data`]: only `Ok` restarts. These drive that exact function with a
/// real cleanup failure (a permission error, not a mock) and check that it
/// reaches the caller instead of a silent `Ok`.
#[cfg(all(test, unix))]
mod caller_tests {
    use super::*;
    use crate::resolve::{confirm_started, prepare, resolve};
    use std::os::unix::fs::PermissionsExt;

    pub(super) struct Env {
        _tmp: tempfile::TempDir,
        pub(super) root: PathBuf,
        pub(super) dirs: SystemDirs,
        pub(super) layout: DataLayout,
    }

    impl Env {
        pub(super) fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let dirs = SystemDirs {
                app_local_data: root.join("local/app"),
                exe_dir: None,
                portable_supported: false,
            };
            let prepared = prepare(&dirs, &resolve(&dirs, None).unwrap()).unwrap();
            confirm_started(&dirs, &prepared).unwrap();
            let layout = prepared.layout;
            Connection::open(&layout.db)
                .unwrap()
                .execute(
                    "INSERT INTO settings(key, value) VALUES ('marker', 'исходная')",
                    [],
                )
                .unwrap();
            fs::create_dir_all(layout.thumbs.join("ab")).unwrap();
            fs::write(layout.thumbs.join("ab/abcd.jpg"), vec![7u8; 5_123]).unwrap();
            Self {
                _tmp: tmp,
                root,
                dirs,
                layout,
            }
        }

        pub(super) fn bootstrap(&self) -> Option<Vec<u8>> {
            fs::read(self.dirs.bootstrap_path()).ok()
        }

        pub(super) fn chosen(&self) -> PathBuf {
            let r = resolve(&self.dirs, None).unwrap();
            prepare(&self.dirs, &r).unwrap().layout.dir
        }
    }

    pub(super) fn plenty(_: &Path) -> io::Result<u64> {
        Ok(1 << 40)
    }

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Root ignores directory modes; then there is no failure to observe.
    fn mode_is_enforced(dir: &Path) -> bool {
        let probe = dir.join("permission-probe");
        let enforced = fs::write(&probe, b"").is_err();
        let _ = fs::remove_file(probe);
        enforced
    }

    pub(super) fn marker(db: &Path) -> String {
        Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
            .query_row("SELECT value FROM settings WHERE key = 'marker'", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    pub(super) fn aside_dirs(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(ASIDE_PREFIX))
            })
            .collect()
    }

    /// el-2xri through the real caller: a folder with a photo swapped onto
    /// the staging name between `mkdir` and registration fails the move
    /// with a message naming it, keeps it and its contents, leaves the
    /// bootstrap and the source alone, and is not "cleaned up" by the abort.
    #[test]
    fn a_staging_name_swapped_before_registration_fails_the_move_and_keeps_it() {
        // The thumbnails have no staging folder of their own any more: they
        // are copied into `thumbs/` inside this one (review el-2ztq8).
        {
            let staging = PARTIAL_DB;
            let env = Env::new();
            let target = env.root.join("target");
            let before = env.bootstrap();
            let own = env.root.join("moved-own-dir");
            let foreign = target.join(staging);
            let result = move_data_with(
                &env.dirs,
                &env.layout,
                Source::System,
                &target,
                plenty,
                &mut |step, at| {
                    if step == Step::Created && at.ends_with(staging) {
                        fs::rename(at, &own).unwrap();
                        fs::create_dir(at).unwrap();
                        fs::write(at.join("foreign-photo.jpg"), b"\xff\xd8 foreign").unwrap();
                        // Private like ours: only its contents give it away.
                        set_mode(at, 0o700);
                    }
                },
            );
            let Err(RelocateError::Copy { reason }) = &result else {
                panic!("{staging}: {result:?}");
            };
            assert!(reason.contains(&foreign.display().to_string()), "{reason}");
            assert!(reason.contains("nothing was removed"), "{reason}");
            assert!(reason.contains("not empty"), "{reason}");
            assert_eq!(
                fs::read(foreign.join("foreign-photo.jpg")).unwrap(),
                b"\xff\xd8 foreign"
            );
            assert!(own.is_dir());
            assert!(aside_dirs(&target).is_empty());
            // What this run did own is gone: the other staging folder, the
            // sidecar reservations, and no database was published.
            assert!(!target.join(DB_FILE).exists());
            let mut names: Vec<_> = fs::read_dir(&target)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                // The writer lock file stays after any run, by design.
                .filter(|n| !n.ends_with(".writer-lock"))
                .collect();
            names.sort();
            assert_eq!(names, [staging], "{staging}");
            assert_eq!(env.bootstrap(), before);
            assert_eq!(env.chosen(), env.layout.dir);
            assert_eq!(marker(&env.layout.db), "исходная");
        }
    }

    #[test]
    fn a_clean_move_commits_and_leaves_no_staging() {
        let env = Env::new();
        let target = env.root.join("target");
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        assert_eq!(env.chosen(), target);
        assert!(!target.join(PARTIAL_DB).exists());
        assert!(aside_dirs(&target).is_empty());
        assert_eq!(marker(&target.join(DB_FILE)), "исходная");
    }

    /// Reproduces el-3xm8's P2: the staging folder cannot be moved aside
    /// (the target became unwritable). Before the fix `copy_data` returned
    /// `staging_left`, `commit` dropped it and the shell restarted on `Ok`.
    #[test]
    fn a_staging_folder_left_in_place_stops_the_switch_and_is_named() {
        let env = Env::new();
        let target = env.root.join("target");
        let before = env.bootstrap();
        let mut enforced = true;
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, at| {
                if step == Step::BeforeMove && at.ends_with(PARTIAL_DB) {
                    set_mode(&target, 0o500);
                    enforced = mode_is_enforced(&target);
                }
            },
        );
        set_mode(&target, 0o700);
        if !enforced {
            return;
        }
        let Err(RelocateError::Incomplete { copy, left }) = &result else {
            panic!("the shell would restart on {result:?}");
        };
        assert_eq!(copy, &target);
        let partial = target.join(PARTIAL_DB);
        assert!(left.contains(&partial.display().to_string()), "{left}");
        assert!(left.contains("ermission denied"), "{left}");
        // The empty private folder that could not be removed is named too.
        let asides = aside_dirs(&target);
        assert_eq!(asides.len(), 1, "{asides:?}");
        assert!(left.contains(&asides[0].display().to_string()), "{left}");
        let text = result.as_ref().unwrap_err().to_string();
        assert!(text.contains(&partial.display().to_string()), "{text}");
        assert!(text.contains(&target.display().to_string()), "{text}");

        // Nothing switched, nothing rolled back, the source untouched.
        assert_eq!(env.bootstrap(), before);
        assert_eq!(env.chosen(), env.layout.dir);
        assert_eq!(marker(&env.layout.db), "исходная");
        assert!(partial.is_dir());
        assert_eq!(marker(&target.join(DB_FILE)), "исходная");
        assert_eq!(
            fs::read(target.join(THUMBS_DIR).join("ab/abcd.jpg")).unwrap(),
            vec![7u8; 5_123]
        );

        // The retained copy stays usable through the explicit action the
        // message names, once the user removed the leftovers.
        fs::remove_dir(&partial).unwrap();
        fs::remove_dir(&asides[0]).unwrap();
        switch_to_existing(&env.dirs, Source::System, &target).unwrap();
        assert_eq!(env.chosen(), target);
    }

    /// The other place cleanup can fail: after the staging folder was moved
    /// aside. The original `.partial` name is then gone, and the leftover is
    /// the private folder — the error must name that, not `.partial`.
    #[test]
    fn a_staging_folder_left_in_the_private_folder_stops_the_switch_and_is_named() {
        let env = Env::new();
        let target = env.root.join("target");
        let before = env.bootstrap();
        let mut aside = None;
        let mut enforced = true;
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, at| {
                if step == Step::BeforeDelete {
                    let dir = at.parent().unwrap().to_path_buf();
                    set_mode(&dir, 0o500);
                    enforced = mode_is_enforced(&dir);
                    aside = Some(dir);
                }
            },
        );
        let aside = aside.expect("cleanup reached the private folder");
        set_mode(&aside, 0o700);
        if !enforced {
            return;
        }
        let Err(RelocateError::Incomplete { copy, left }) = &result else {
            panic!("the shell would restart on {result:?}");
        };
        assert_eq!(copy, &target);
        let moved = aside.join(PARTIAL_DB);
        assert!(left.contains(&moved.display().to_string()), "{left}");
        assert!(left.contains("ermission denied"), "{left}");
        assert!(moved.is_dir());
        assert!(!target.join(PARTIAL_DB).exists());
        assert_eq!(env.bootstrap(), before);
        assert_eq!(env.chosen(), env.layout.dir);
        assert_eq!(marker(&target.join(DB_FILE)), "исходная");
    }

    /// `commit` itself refuses a copy that carries a cleanup failure, so a
    /// caller composing `copy_data` and `commit` by hand cannot lose it.
    #[test]
    fn commit_refuses_a_copy_with_staging_left() {
        let env = Env::new();
        let target = env.root.join("target");
        let mut copied =
            copy_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        assert!(copied.staging_left.is_none());
        copied.staging_left = Some("injected leftover".into());
        let before = env.bootstrap();
        let error = copied.commit().unwrap_err();
        assert_eq!(
            error,
            RelocateError::Incomplete {
                copy: target.clone(),
                left: "injected leftover".into()
            }
        );
        assert_eq!(env.bootstrap(), before);
        assert_eq!(marker(&target.join(DB_FILE)), "исходная");
    }
}

/// Review el-2ztq8 (B1): after the database was verified, the whole
/// registered staging folder was renamed away inside the shared target and a
/// foreign folder put under its name; publication by path then moved the
/// foreign `photo-cleanup.db` to the final name and the caller was told the
/// copy was "complete and verified". Publication now moves entries out of
/// the open staging folder into the open target folder only. These drive
/// [`move_data`] (the shell's entry point) with the renames at each point
/// between registration and publication, and with the target itself
/// renamed before and after its path check.
#[cfg(all(test, unix))]
mod publication_tests {
    use super::caller_tests::{aside_dirs, marker, plenty, Env};
    use super::*;
    use std::collections::BTreeMap;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    #[cfg(target_os = "macos")]
    use std::process::Command;

    pub(super) const FOREIGN_DB: &[u8] = b"FOREIGN DATABASE 91073";
    const FOREIGN_THUMB: &[u8] = b"\xff\xd8 foreign thumbnail 55120";

    /// Everything that identifies an entry and its contents: inode, mode,
    /// owner, modification time, bytes, extended attributes (macOS).
    pub(super) type Signature = (u64, u32, u32, u32, i64, i64, Option<Vec<u8>>, Vec<u8>);

    pub(super) fn signature(path: &Path) -> Signature {
        let m = fs::symlink_metadata(path).unwrap();
        let bytes = m.is_file().then(|| fs::read(path).unwrap());
        #[cfg(target_os = "macos")]
        let xattrs = Command::new("xattr")
            .arg("-l")
            .arg(path)
            .output()
            .unwrap()
            .stdout;
        #[cfg(not(target_os = "macos"))]
        let xattrs = Vec::new();
        let is_dir = m.is_dir();
        (
            m.ino(),
            m.mode(),
            m.uid(),
            m.gid(),
            // A directory's times change when entries are added to it
            // (not ours to add); a file's must not change at all.
            if is_dir { 0 } else { m.mtime() },
            if is_dir { 0 } else { m.mtime_nsec() },
            bytes,
            xattrs,
        )
    }

    /// Relative path → signature of `dir` and everything under it.
    pub(super) fn tree(dir: &Path) -> BTreeMap<PathBuf, Signature> {
        let mut out = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            out.insert(d.strip_prefix(dir).unwrap().to_path_buf(), signature(&d));
            if fs::symlink_metadata(&d).unwrap().is_dir() {
                for e in fs::read_dir(&d).unwrap() {
                    stack.push(e.unwrap().path());
                }
            }
        }
        out
    }

    /// A target that other accounts may add to and rename in, as in the
    /// review: on macOS an inherited `everyone` ACL entry, elsewhere mode
    /// 0777. The renames below are made by this user; the permission is
    /// what would let another account make them.
    pub(super) fn shared_target(env: &Env) -> PathBuf {
        let target = env.root.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&env.root, fs::Permissions::from_mode(0o755)).unwrap();
        #[cfg(target_os = "macos")]
        {
            let status = Command::new("chmod")
                .args([
                    "+a",
                    "everyone allow list,search,add_file,add_subdirectory,delete_child,\
                     file_inherit,directory_inherit",
                ])
                .arg(&target)
                .status()
                .unwrap();
            assert!(status.success());
        }
        #[cfg(not(target_os = "macos"))]
        fs::set_permissions(&target, fs::Permissions::from_mode(0o777)).unwrap();
        target
    }

    /// A target only this user can change: the protected namespace the
    /// move requires. The renames in these tests are made by this same
    /// user — outside the threat model, which the admission keeps other
    /// accounts out of — and show that even then what is published,
    /// verified and chosen is decided by objects, not names.
    pub(super) fn own_target(env: &Env) -> PathBuf {
        let target = env.root.join("target");
        fs::create_dir(&target).unwrap();
        target
    }

    /// A foreign folder, prepared beforehand in `parent`: open to everyone,
    /// with a `photo-cleanup.db` that is not SQLite (optional), a thumbnail
    /// and an extended attribute on each file (macOS).
    pub(super) fn foreign(parent: &Path, name: &str, with_db: bool) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir_all(dir.join("thumbs/ab")).unwrap();
        let mut files = vec![dir.join("thumbs/ab/abcd.jpg")];
        fs::write(&files[0], FOREIGN_THUMB).unwrap();
        if with_db {
            files.push(dir.join(DB_FILE));
            fs::write(&files[1], FOREIGN_DB).unwrap();
        }
        #[cfg(target_os = "macos")]
        for f in &files {
            let status = Command::new("xattr")
                .args(["-w", "com.photo-cleanup.fixture", "preserve"])
                .arg(f)
                .status()
                .unwrap();
            assert!(status.success());
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        dir
    }

    pub(super) fn own_thumbs(dir: &Path) -> Vec<u8> {
        fs::read(dir.join(THUMBS_DIR).join("ab/abcd.jpg")).unwrap()
    }

    /// B1 verbatim at the two points after the copies are proven (before
    /// and after the target's path check). The foreign folder keeps every
    /// entry, byte, inode, mode and attribute at its own place; the proven
    /// copy — not the foreign file — is what gets published; the caller is
    /// told where this run's own (now empty) staging folder went and that
    /// the entry at the staging name was not touched, and the copy it calls
    /// verified really is the one that was verified.
    #[test]
    fn a_staging_folder_replaced_after_verification_is_not_published() {
        for when in [Step::Verified, Step::Publish] {
            let env = Env::new();
            let target = own_target(&env);
            let planted = foreign(&target, "foreign-staging", true);
            let before = tree(&planted);
            let boot = env.bootstrap();
            let partial = target.join(PARTIAL_DB);
            let saved = target.join("saved-db-staging");
            let result = move_data_with(
                &env.dirs,
                &env.layout,
                Source::System,
                &target,
                plenty,
                &mut |step, _| {
                    if step == when {
                        fs::rename(&partial, &saved).unwrap();
                        fs::rename(&planted, &partial).unwrap();
                    }
                },
            );
            let Err(RelocateError::Incomplete { copy, left }) = &result else {
                panic!("{when:?}: {result:?}");
            };
            assert_eq!(copy, &target);
            // The foreign folder is untouched where it was put.
            assert_eq!(tree(&partial), before, "{when:?}");
            // What was published is the proven copy of the source.
            assert_eq!(marker(&target.join(DB_FILE)), "исходная");
            assert_eq!(own_thumbs(&target), vec![7u8; 5_123]);
            // This run's own staging folder is empty where it was moved to,
            // and named, as is the foreign entry at the staging name.
            assert_eq!(fs::read_dir(&saved).unwrap().count(), 0);
            assert!(left.contains(&saved.display().to_string()), "{left}");
            assert!(left.contains(&partial.display().to_string()), "{left}");
            assert!(left.contains("replaced"), "{left}");
            let text = result.as_ref().unwrap_err().to_string();
            assert!(text.contains(&saved.display().to_string()), "{text}");
            assert!(aside_dirs(&target).is_empty());
            assert_eq!(env.bootstrap(), boot);
            assert_eq!(env.chosen(), env.layout.dir);
            assert_eq!(marker(&env.layout.db), "исходная");
            // The copy the message offers is usable as it says.
            switch_to_existing(&env.dirs, Source::System, &target).unwrap();
            assert_eq!(env.chosen(), target);
            assert_eq!(tree(&partial), before);
        }
    }

    /// The same rename before the database is written (review el-59w6z,
    /// B1). The database file is created by this run in the open staging
    /// folder, so it goes with that folder; SQLite may only write while the
    /// path leads to that very file, which it no longer does. So nothing is
    /// written into the foreign folder — with or without a foreign
    /// `photo-cleanup.db` in it — nothing is published, every foreign entry
    /// keeps its bytes and metadata, and the error names where this run's
    /// own file and staging folder are now.
    #[test]
    fn a_staging_folder_replaced_before_the_copy_publishes_nothing() {
        for with_db in [true, false] {
            let env = Env::new();
            let target = own_target(&env);
            let planted = foreign(&target, "foreign-staging", with_db);
            let before = tree(&planted);
            let boot = env.bootstrap();
            let partial = target.join(PARTIAL_DB);
            let saved = target.join("saved-db-staging");
            let result = move_data_with(
                &env.dirs,
                &env.layout,
                Source::System,
                &target,
                plenty,
                &mut |step, _| {
                    if step == Step::Registered {
                        fs::rename(&partial, &saved).unwrap();
                        fs::rename(&planted, &partial).unwrap();
                    }
                },
            );
            let Err(RelocateError::Cleanup { error, left }) = &result else {
                panic!("with_db={with_db}: {result:?}");
            };
            assert!(matches!(**error, RelocateError::Copy { .. }), "{error:?}");
            let text = result.as_ref().unwrap_err().to_string();
            let mut after = tree(&partial);
            after.remove(Path::new(""));
            let mut expected = before.clone();
            expected.remove(Path::new(""));
            assert_eq!(after, expected, "with_db={with_db}");
            assert_eq!(
                partial.join(DB_FILE).exists(),
                with_db,
                "no database was disclosed into the foreign folder"
            );
            assert!(text.contains("Nothing was published"), "{text}");
            assert!(
                text.contains(&saved.join(DB_FILE).display().to_string()),
                "{text}"
            );
            assert!(
                left.join("; ").contains(&saved.display().to_string()),
                "{left:?}"
            );
            assert!(!target.join(DB_FILE).exists());
            assert!(!target.join(THUMBS_DIR).exists());
            assert_eq!(env.bootstrap(), boot);
            assert_eq!(marker(&env.layout.db), "исходная");
        }
    }

    /// The target folder itself renamed away and a foreign one put at its
    /// path, before the path check: nothing is published anywhere, the
    /// foreign folder keeps everything, and this run's leftovers in the
    /// renamed folder are named with where they are now.
    #[test]
    fn a_target_replaced_before_publication_publishes_nothing() {
        let env = Env::new();
        let target = own_target(&env);
        let moved = env.root.join("target-moved");
        let boot = env.bootstrap();
        let mut before = None;
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, _| {
                if step == Step::Verified {
                    fs::rename(&target, &moved).unwrap();
                    let planted = foreign(&env.root, "target", true);
                    before = Some(tree(&planted));
                }
            },
        );
        let Err(RelocateError::Cleanup { error, left }) = &result else {
            panic!("{result:?}");
        };
        let RelocateError::Copy { reason } = &**error else {
            panic!("{error:?}");
        };
        assert!(reason.contains(&target.display().to_string()), "{reason}");
        assert!(reason.contains(&moved.display().to_string()), "{reason}");
        assert_eq!(Some(tree(&target)), before);
        assert!(!moved.join(DB_FILE).exists());
        assert!(!moved.join(THUMBS_DIR).exists());
        // Our staging folder and the empty reservations stay in the moved
        // folder, each named where it is now.
        let left = left.join("; ");
        assert!(
            left.contains(&moved.join(PARTIAL_DB).display().to_string()),
            "{left}"
        );
        assert!(
            left.contains(&moved.join(format!("{DB_FILE}-wal")).display().to_string()),
            "{left}"
        );
        assert_eq!(env.bootstrap(), boot);
        assert_eq!(marker(&env.layout.db), "исходная");
    }

    /// The target renamed between `mkdir` of the staging folder and its
    /// registration, with our staging folder carried into a new folder at
    /// the target's path: registration by name succeeds, but the folder is
    /// not in the target opened for the move, so the run stops before
    /// SQLite writes anything, and nothing is published in either folder.
    #[test]
    fn a_staging_folder_outside_the_opened_target_is_refused() {
        let env = Env::new();
        let target = own_target(&env);
        let moved = env.root.join("target-moved");
        let boot = env.bootstrap();
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, at| {
                if step == Step::Created && at.ends_with(PARTIAL_DB) {
                    fs::rename(&target, &moved).unwrap();
                    fs::create_dir(&target).unwrap();
                    fs::rename(moved.join(PARTIAL_DB), target.join(PARTIAL_DB)).unwrap();
                }
            },
        );
        let text = result.as_ref().unwrap_err().to_string();
        assert!(
            text.contains("is no longer the one opened for the move"),
            "{text}"
        );
        assert!(text.contains("nothing was published"), "{text}");
        for dir in [&target, &moved] {
            assert!(!dir.join(DB_FILE).exists());
            assert!(!dir.join(THUMBS_DIR).exists());
        }
        assert_eq!(env.bootstrap(), boot);
        assert_eq!(marker(&env.layout.db), "исходная");
    }

    /// The same rename right after the path check, before the moves: the
    /// proven copy goes into the folder that was checked (now elsewhere),
    /// never into the foreign one, and the caller is not switched to the
    /// path: [`RelocateError::Displaced`] names both places.
    #[test]
    fn a_target_replaced_during_publication_is_not_switched_to() {
        let env = Env::new();
        let target = own_target(&env);
        let moved = env.root.join("target-moved");
        let boot = env.bootstrap();
        let mut before = None;
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, _| {
                if step == Step::Publish {
                    fs::rename(&target, &moved).unwrap();
                    let planted = foreign(&env.root, "target", true);
                    before = Some(tree(&planted));
                }
            },
        );
        let Err(RelocateError::Displaced {
            target: named,
            reason,
        }) = &result
        else {
            panic!("{result:?}");
        };
        assert_eq!(named, &target);
        assert!(reason.contains(&moved.display().to_string()), "{reason}");
        assert_eq!(Some(tree(&target)), before);
        assert_eq!(marker(&moved.join(DB_FILE)), "исходная");
        assert_eq!(own_thumbs(&moved), vec![7u8; 5_123]);
        let text = result.as_ref().unwrap_err().to_string();
        assert!(!text.contains("complete and verified"), "{text}");
        assert_eq!(env.bootstrap(), boot);
        assert_eq!(env.chosen(), env.layout.dir);
    }
}

/// The whole lifecycle of a move under the protected-namespace contract
/// (el-2xri; review el-59w6z, diagnosis el-49o3y): admission refuses
/// namespaces other accounts could redirect before anything is written; in
/// an admitted one the copy is bound to the objects that were proven, from
/// the snapshot through publication, the bootstrap and every later start.
///
/// The hooks rename entries as this same user. In an admitted namespace no
/// other account can make those renames (that is what admission proves);
/// the tests show that even then nothing foreign is written, verified,
/// published or chosen.
#[cfg(all(test, unix))]
mod lifecycle_tests {
    use super::caller_tests::{marker, plenty, Env};
    use super::publication_tests::{foreign, own_target, shared_target, signature, tree};
    use super::*;
    use crate::resolve::{confirm_started, prepare, resolve};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    #[cfg(target_os = "macos")]
    use std::process::Command;

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Every entry under `dir` with its signature, or an empty map.
    fn snapshot_of(
        dir: &Path,
    ) -> std::collections::BTreeMap<PathBuf, super::publication_tests::Signature> {
        if dir.exists() {
            tree(dir)
        } else {
            Default::default()
        }
    }

    fn xattr(path: &Path) {
        #[cfg(target_os = "macos")]
        assert!(Command::new("xattr")
            .args(["-w", "com.photo-cleanup.fixture", "keep"])
            .arg(path)
            .status()
            .unwrap()
            .success());
        #[cfg(not(target_os = "macos"))]
        let _ = path;
    }

    /// A valid photo-cleanup database with the source's schema and row
    /// counts, but another marker: what a substitute passing every count
    /// would look like.
    fn plausible_foreign_db(env: &Env, at: &Path) {
        let src = Connection::open(&env.layout.db).unwrap();
        src.execute("VACUUM INTO ?1", [at.to_str().unwrap()])
            .unwrap();
        Connection::open(at)
            .unwrap()
            .execute(
                "UPDATE settings SET value = 'чужая' WHERE key = 'marker'",
                [],
            )
            .unwrap();
    }

    fn unprotected(blockers: &[Blocker], role: FolderRole) -> (&Path, &str) {
        blockers
            .iter()
            .find_map(|b| match b {
                Blocker::UnprotectedFolder {
                    role: r,
                    component,
                    reason,
                    ..
                } if *r == role => Some((component.as_path(), reason.as_str())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {role:?} refusal: {blockers:?}"))
    }

    /// Folders other accounts could change — a target open to everyone
    /// (inherited `everyone` ACL on macOS, mode 0777 elsewhere) holding a
    /// foreign folder with an empty `photo-cleanup.db`, a private target
    /// inside such a folder, a folder still to be made there, a
    /// group-writable target, and the current data and settings folders
    /// made group-writable — are refused by the preview and by the move,
    /// twice, naming the folder and the reason. Nothing is written: every
    /// foreign entry keeps bytes, inode, mode, owner, time and attributes,
    /// the source folder gains no writer lock or anything else, the
    /// bootstrap keeps its bytes, and no reservation or staging appears.
    /// The refusal comes before any later step, so these cases exercise
    /// the admission only, not the later hooks.
    #[test]
    fn unprotected_namespaces_are_refused_before_anything_is_written() {
        for case in [
            "shared target",
            "inside shared",
            "missing inside shared",
            "group-writable",
            "source",
            "settings",
        ] {
            let env = Env::new();
            let (target, watched, role, component): (PathBuf, PathBuf, FolderRole, PathBuf) =
                match case {
                    "shared target" => {
                        let t = shared_target(&env);
                        let f = foreign(&t, "foreign-staging", false);
                        let empty = f.join(DB_FILE);
                        fs::write(&empty, []).unwrap();
                        xattr(&empty);
                        (t.clone(), t.clone(), FolderRole::Target, t)
                    }
                    "inside shared" | "missing inside shared" => {
                        let shared = shared_target(&env);
                        foreign(&shared, "neighbour", true);
                        let t = if case == "inside shared" {
                            let t = shared.join("mine");
                            fs::create_dir(&t).unwrap();
                            set_mode(&t, 0o700);
                            t
                        } else {
                            shared.join("new/data")
                        };
                        (t, shared.clone(), FolderRole::Target, shared)
                    }
                    "group-writable" => {
                        let t = own_target(&env);
                        foreign(&t, "neighbour", true);
                        set_mode(&t, 0o775);
                        (t.clone(), t.clone(), FolderRole::Target, t)
                    }
                    "source" => {
                        set_mode(&env.layout.dir, 0o775);
                        let t = own_target(&env);
                        (t.clone(), t, FolderRole::Source, env.layout.dir.clone())
                    }
                    _ => {
                        set_mode(&env.dirs.app_local_data, 0o770);
                        let t = own_target(&env);
                        (
                            t.clone(),
                            t,
                            FolderRole::Settings,
                            env.dirs.app_local_data.clone(),
                        )
                    }
                };
            let before = snapshot_of(&watched);
            let source_before = tree(&env.layout.dir);
            let boot = env.bootstrap();
            for attempt in 0..2 {
                let preview =
                    preview_move_with(&env.dirs, &env.layout, Source::System, &target, plenty);
                let (named, reason) = unprotected(&preview.blockers, role);
                assert_eq!(named, component, "{case} {attempt}: {reason}");
                assert!(
                    preview
                        .reasons
                        .iter()
                        .any(|r| r.contains(&component.display().to_string()) && r.contains(reason)),
                    "{case}: {:?}",
                    preview.reasons
                );
                let result = move_data(&env.dirs, &env.layout, Source::System, &target, plenty);
                let Err(RelocateError::Blocked { blockers }) = &result else {
                    panic!("{case} {attempt}: {result:?}");
                };
                assert_eq!(unprotected(blockers, role), (named, reason), "{case}");
                assert_eq!(snapshot_of(&watched), before, "{case} {attempt}");
                assert_eq!(tree(&env.layout.dir), source_before, "{case} {attempt}");
                assert_eq!(env.bootstrap(), boot, "{case} {attempt}");
                assert!(!target.join(PARTIAL_DB).exists());
                assert!(!target.join(format!("{DB_FILE}-wal")).exists());
            }
            if case == "missing inside shared" {
                assert!(!target.parent().unwrap().exists());
            }
            assert_eq!(marker(&env.layout.db), "исходная");
        }
    }

    /// The five findings of review el-59w6z, one by one, in an admitted
    /// target. 1 — an empty foreign `photo-cleanup.db` in a folder swapped
    /// onto the staging name after registration is never written to: it
    /// keeps its size, inode, time, mode and attributes.
    #[test]
    fn an_empty_foreign_database_is_never_filled() {
        let env = Env::new();
        let target = own_target(&env);
        let planted = foreign(&target, "foreign-staging", false);
        let empty = planted.join(DB_FILE);
        fs::write(&empty, []).unwrap();
        set_mode(&empty, 0o666);
        xattr(&empty);
        let before = signature(&empty);
        let boot = env.bootstrap();
        let partial = target.join(PARTIAL_DB);
        let saved = target.join("saved-private-staging");
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, _| {
                if step == Step::Registered {
                    fs::rename(&partial, &saved).unwrap();
                    fs::rename(&planted, &partial).unwrap();
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(signature(&partial.join(DB_FILE)), before);
        assert_eq!(fs::metadata(partial.join(DB_FILE)).unwrap().len(), 0);
        assert_eq!(env.bootstrap(), boot);
        assert_eq!(marker(&env.layout.db), "исходная");
    }

    /// 3 — the published database or thumbnail folder renamed away after
    /// publication and a foreign one put at its name: the commit refuses
    /// (the objects at the names are not the proven ones), the bootstrap
    /// stays, the foreign entry is untouched, and the error says where the
    /// verified copy is now.
    #[test]
    fn a_replaced_published_database_or_thumbnail_folder_is_not_chosen() {
        for name in [DB_FILE, THUMBS_DIR] {
            let env = Env::new();
            let target = own_target(&env);
            let foreign_dir = foreign(&target, "foreign", true);
            let planted = foreign_dir.join(name);
            let before = signature(&planted);
            let saved = target.join(format!("saved-{name}"));
            let boot = env.bootstrap();
            let mut swapped = false;
            let result = move_data_with(
                &env.dirs,
                &env.layout,
                Source::System,
                &target,
                plenty,
                &mut |step, _| {
                    if step == Step::BeforeMove && !swapped && target.join(DB_FILE).exists() {
                        fs::rename(target.join(name), &saved).unwrap();
                        fs::rename(&planted, target.join(name)).unwrap();
                        swapped = true;
                    }
                },
            );
            assert!(swapped);
            let Err(RelocateError::Displaced { reason, .. }) = &result else {
                panic!("{name}: {result:?}");
            };
            assert!(reason.contains(&saved.display().to_string()), "{reason}");
            assert_eq!(signature(&target.join(name)), before, "{name}");
            assert_eq!(env.bootstrap(), boot, "{name}");
            assert_eq!(env.chosen(), env.layout.dir);
            let text = result.as_ref().unwrap_err().to_string();
            assert!(!text.contains("complete and verified"), "{text}");
        }
    }

    /// 4 — the whole target renamed away and a foreign one put at its path
    /// while the commit reads the current bootstrap. A disposable FIFO at
    /// the bootstrap's name makes that read wait for the substitution
    /// deterministically (it models slow I/O; only parent entries are
    /// renamed). The bootstrap is still that FIFO afterwards — never
    /// replaced — the foreign tree is untouched, and the verified copy is
    /// named where it is now.
    #[test]
    fn a_target_substituted_while_the_bootstrap_is_read_is_not_chosen() {
        use std::io::Write;
        let env = Env::new();
        let target = own_target(&env);
        let planted = foreign(&env.root, "foreign-target", true);
        let before = tree(&planted);
        let saved = env.root.join("saved-target");
        let gate = SystemDirs {
            app_local_data: env.root.join("gated-bootstrap"),
            exe_dir: None,
            portable_supported: false,
        };
        fs::create_dir_all(&gate.app_local_data).unwrap();
        let copied = copy_data(&gate, &env.layout, Source::System, &target, plenty).unwrap();
        let bootstrap = gate.bootstrap_path();
        let c = std::ffi::CString::new(bootstrap.to_str().unwrap()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let (t, sv, fifo) = (target.clone(), saved.clone(), bootstrap.clone());
        let worker = std::thread::spawn(move || {
            let mut w = fs::OpenOptions::new().write(true).open(&fifo).unwrap();
            fs::rename(&t, &sv).unwrap();
            fs::rename(&planted, &t).unwrap();
            w.write_all(br#"{"version":1,"mode":"system","data_dir":null}"#)
                .unwrap();
        });
        let result = copied.commit();
        worker.join().unwrap();
        let Err(RelocateError::Displaced { reason, .. }) = &result else {
            panic!("{result:?}");
        };
        assert!(reason.contains(&saved.display().to_string()), "{reason}");
        assert!(fs::symlink_metadata(&bootstrap)
            .unwrap()
            .file_type()
            .is_fifo());
        let temporaries: Vec<_> = fs::read_dir(&gate.app_local_data)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(temporaries.is_empty(), "{temporaries:?}");
        assert_eq!(tree(&target), before);
        assert_eq!(marker(&saved.join(DB_FILE)), "исходная");
    }

    /// 5 — staging cleanup fails because the staging name was replaced,
    /// and the published database was replaced as well: the error never
    /// calls the foreign bytes a complete and verified copy, never suggests
    /// using the folder, and says where the real one is.
    #[test]
    fn a_leftover_never_makes_a_foreign_database_verified() {
        let env = Env::new();
        let target = own_target(&env);
        let foreign_dir = foreign(&target, "foreign", true);
        let foreign_db = foreign_dir.join(DB_FILE);
        let before_db = signature(&foreign_db);
        let fake_staging = foreign(&target, "foreign-partial", false);
        let before_partial = tree(&fake_staging);
        let saved_db = target.join("saved-verified-db");
        let saved_partial = target.join("saved-own-partial");
        let boot = env.bootstrap();
        let mut swapped = false;
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, path| {
                if step == Step::BeforeMove && !swapped && path.ends_with(PARTIAL_DB) {
                    fs::rename(target.join(DB_FILE), &saved_db).unwrap();
                    fs::rename(&foreign_db, target.join(DB_FILE)).unwrap();
                    fs::rename(path, &saved_partial).unwrap();
                    fs::rename(&fake_staging, path).unwrap();
                    swapped = true;
                }
            },
        );
        assert!(swapped);
        assert_eq!(signature(&target.join(DB_FILE)), before_db);
        assert_eq!(tree(&target.join(PARTIAL_DB)), before_partial);
        assert_eq!(env.bootstrap(), boot);
        let Err(RelocateError::Displaced { reason, .. }) = &result else {
            panic!("{result:?}");
        };
        assert!(reason.contains(&saved_db.display().to_string()), "{reason}");
        let text = result.as_ref().unwrap_err().to_string();
        assert!(!text.contains("complete and verified"), "{text}");
        assert!(!text.contains("as an existing folder"), "{text}");
        assert_eq!(marker(&saved_db), "исходная");
    }

    /// Verification reads this run's own file, not whatever is at its
    /// name: the staging folder renamed away just before verification and
    /// a folder with a plausible foreign database (same schema and counts,
    /// another marker) left at its name — no swap back. The move fails,
    /// nothing is published, the foreign database is untouched.
    #[test]
    fn verification_is_not_satisfied_by_a_substitute_at_the_name() {
        let env = Env::new();
        let target = own_target(&env);
        let planted = target.join("foreign-staging");
        fs::create_dir(&planted).unwrap();
        plausible_foreign_db(&env, &planted.join(DB_FILE));
        let before = tree(&planted);
        let boot = env.bootstrap();
        let partial = target.join(PARTIAL_DB);
        let saved = target.join("saved-private-staging");
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, _| {
                if step == Step::BeforeVerify {
                    fs::rename(&partial, &saved).unwrap();
                    fs::rename(&planted, &partial).unwrap();
                }
            },
        );
        let text = result.as_ref().unwrap_err().to_string();
        assert!(
            text.contains(&saved.join(DB_FILE).display().to_string()),
            "{text}"
        );
        assert_eq!(tree(&partial), before);
        assert!(!target.join(DB_FILE).exists());
        assert_eq!(env.bootstrap(), boot);
    }

    /// The other half of the same proof: this run's own snapshot corrupted
    /// and a valid foreign database put at its name before verification.
    /// The verification must not pass on the foreign one. On macOS it reads
    /// the own file (through its descriptor) and fails as a verification
    /// error; elsewhere the path check after it refuses.
    #[test]
    fn a_corrupt_own_snapshot_is_not_rescued_by_a_valid_substitute() {
        use std::io::{Seek, Write};
        let env = Env::new();
        let target = own_target(&env);
        let planted = target.join("foreign-staging");
        fs::create_dir(&planted).unwrap();
        plausible_foreign_db(&env, &planted.join(DB_FILE));
        let before = tree(&planted);
        let partial = target.join(PARTIAL_DB);
        let saved = target.join("saved-private-staging");
        let result = move_data_with(
            &env.dirs,
            &env.layout,
            Source::System,
            &target,
            plenty,
            &mut |step, at| {
                if step == Step::BeforeVerify {
                    let mut own = fs::OpenOptions::new().write(true).open(at).unwrap();
                    own.seek(io::SeekFrom::Start(0)).unwrap();
                    own.write_all(&[0xA5; 512]).unwrap();
                    own.sync_all().unwrap();
                    fs::rename(&partial, &saved).unwrap();
                    fs::rename(&planted, &partial).unwrap();
                }
            },
        );
        let Err(error) = &result else {
            panic!("a corrupt copy was accepted: {result:?}");
        };
        let inner = match error {
            RelocateError::Cleanup { error, .. } => &**error,
            e => e,
        };
        #[cfg(target_os = "macos")]
        assert!(matches!(inner, RelocateError::Verify { .. }), "{inner:?}");
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(inner, RelocateError::Copy { .. }), "{inner:?}");
        assert_eq!(tree(&partial), before);
        assert!(!target.join(DB_FILE).exists());
    }

    /// A move into an admitted folder is recorded bound — format 2, with
    /// volume, inodes and generation — and the next starts open exactly
    /// those objects, confirm, and keep the binding. A source without a
    /// thumbnail cache still gets a (empty) bound `thumbs/`.
    #[test]
    fn a_moved_copy_is_bound_and_starts_on_its_own_objects() {
        for thumbs in [true, false] {
            let env = Env::new();
            if !thumbs {
                fs::remove_dir_all(&env.layout.thumbs).unwrap();
            }
            let target = own_target(&env);
            move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
            let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
            assert_eq!(b.version, crate::bootstrap::BOOTSTRAP_VERSION);
            let binding = b.current.binding.as_deref().cloned().expect("bound");
            assert!(binding.db > 0 && binding.thumbs > 0 && binding.dir > 0);
            assert_eq!(binding.generation.len(), 32);
            assert!(b.previous.as_ref().is_some_and(|p| p.binding.is_none()));
            for _ in 0..2 {
                let r = resolve(&env.dirs, None).unwrap();
                let p = prepare(&env.dirs, &r).unwrap();
                assert!(p.guard.is_some());
                p.verify_binding().unwrap();
                confirm_started(&env.dirs, &p).unwrap();
                assert_eq!(p.layout.dir, target);
                assert_eq!(marker(&p.layout.db), "исходная");
            }
            let b = read_bootstrap(&env.dirs.bootstrap_path()).unwrap().unwrap();
            assert!(b.previous.is_none());
            assert_eq!(b.current.binding.as_deref(), Some(&binding));
            assert!(target.join(THUMBS_DIR).is_dir());
        }
    }

    /// After a successful move, what is at the bound path is substituted
    /// before the next start — the database (a byte copy: another inode, no
    /// generation), the thumbnail folder, the whole folder, or only the
    /// generation removed. Every start is refused before SQLite: no `-wal`,
    /// `-shm` or migration touches the foreign files, nothing is created,
    /// the bootstrap is unchanged, and "go back" is offered.
    #[test]
    fn a_bound_copy_substituted_before_a_start_is_refused_before_sqlite() {
        for case in ["database", "thumbs", "folder", "generation"] {
            let env = Env::new();
            let target = own_target(&env);
            move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
            let boot = env.bootstrap();
            let saved = env.root.join("saved");
            match case {
                "database" => {
                    fs::rename(target.join(DB_FILE), &saved).unwrap();
                    fs::copy(&saved, target.join(DB_FILE)).unwrap();
                }
                "thumbs" => {
                    fs::rename(target.join(THUMBS_DIR), &saved).unwrap();
                    fs::create_dir(target.join(THUMBS_DIR)).unwrap();
                }
                "folder" => {
                    fs::rename(&target, &saved).unwrap();
                    fs::create_dir(&target).unwrap();
                    fs::copy(saved.join(DB_FILE), target.join(DB_FILE)).unwrap();
                    fs::create_dir(target.join(THUMBS_DIR)).unwrap();
                }
                _ => {
                    #[cfg(target_os = "macos")]
                    assert!(Command::new("xattr")
                        .args(["-d", "io.github.imcitius.photo-cleanup.generation"])
                        .arg(target.join(DB_FILE))
                        .status()
                        .unwrap()
                        .success());
                    #[cfg(target_os = "linux")]
                    {
                        let c =
                            std::ffi::CString::new(target.join(DB_FILE).to_str().unwrap()).unwrap();
                        // SAFETY: valid NUL-terminated strings.
                        assert_eq!(
                            unsafe {
                                libc::removexattr(
                                    c.as_ptr(),
                                    c"user.photo-cleanup.generation".as_ptr(),
                                )
                            },
                            0
                        );
                    }
                }
            }
            let before = tree(&target);
            for _ in 0..2 {
                let err = resolve(&env.dirs, None).unwrap_err();
                let StartupError::DataUnavailable { why, previous, .. } = &err else {
                    panic!("{case}: {err:?}");
                };
                assert!(
                    matches!(why, crate::Unavailable::NotTheBoundCopy(_)),
                    "{case}: {why:?}"
                );
                assert!(previous.is_some(), "{case}");
                assert_eq!(tree(&target), before, "{case}");
                assert_eq!(env.bootstrap(), boot, "{case}");
            }
        }
    }

    /// The gaps inside a start: a substitution between `resolve` and
    /// `prepare` is refused by `prepare` before SQLite; one between
    /// `prepare` and the server's own open is refused by that open — the
    /// server opens through the guard ([`pc_db::Db::open_bound`]), so the
    /// replacement is not written to (el-146id B1; the real server is in
    /// `storage_tests`).
    #[test]
    fn a_substitution_during_a_start_is_caught_at_each_step() {
        let env = Env::new();
        let target = own_target(&env);
        move_data(&env.dirs, &env.layout, Source::System, &target, plenty).unwrap();
        let saved = env.root.join("saved.db");

        let r = resolve(&env.dirs, None).unwrap();
        fs::rename(target.join(DB_FILE), &saved).unwrap();
        fs::copy(&saved, target.join(DB_FILE)).unwrap();
        let before = tree(&target);
        let err = prepare(&env.dirs, &r).unwrap_err();
        assert!(err.to_string().contains("not the copy"), "{err}");
        assert_eq!(tree(&target), before);
        fs::remove_file(target.join(DB_FILE)).unwrap();
        fs::rename(&saved, target.join(DB_FILE)).unwrap();

        let r = resolve(&env.dirs, None).unwrap();
        let p = prepare(&env.dirs, &r).unwrap();
        p.verify_binding().unwrap();
        fs::rename(target.join(DB_FILE), &saved).unwrap();
        fs::copy(&saved, target.join(DB_FILE)).unwrap();
        let before = tree(&target);
        // What the server does with the path and the guard it is given.
        let guard = p.storage_binding().unwrap();
        let Err(refused) = pc_db::Db::open_bound(&p.layout.db, guard.as_ref()) else {
            panic!("the replacement was opened");
        };
        assert!(
            format!("{refused:#}").contains("nothing was written"),
            "{refused:#}"
        );
        assert_eq!(tree(&target), before);
        let err = p.verify_binding().unwrap_err();
        let StartupError::DataUnavailable { why, .. } = &err else {
            panic!("{err:?}");
        };
        assert!(
            matches!(why, crate::Unavailable::NotTheBoundCopy(_)),
            "{why:?}"
        );
    }

    /// Format 1 files keep working unbound, as written by older builds; a
    /// binding inside a format 1 file is not a format 1 file.
    #[test]
    fn unbound_and_malformed_bindings_are_told_apart() {
        let env = Env::new();
        let path = env.dirs.bootstrap_path();
        let b = read_bootstrap(&path).unwrap().unwrap();
        assert_eq!(b.version, 1);
        assert!(b.current.binding.is_none());
        let text = String::from_utf8(fs::read(&path).unwrap()).unwrap();
        assert!(!text.contains("binding"));
        fs::write(
            &path,
            br#"{"version":1,"mode":"system","data_dir":null,"binding":{"volume":"x","dir":1,"db":2,"thumbs":3,"generation":"g"}}"#,
        )
        .unwrap();
        assert!(matches!(
            read_bootstrap(&path),
            Err(StartupError::BootstrapUnreadable { .. })
        ));
    }
}

/// el-2xri: the boundary between `mkdir` and taking the new directory's
/// identity. Before the fix the entry at the name after `mkdir` was
/// registered whatever it was, so a folder swapped in there was later
/// deleted with its contents as "ours" (reproduced in el-3xm8).
///
/// Tests run as one user, so a swap "by another user" is simulated with the
/// same user's entries; the owner check itself is exercised through
/// [`fresh_private_dir`] with a different effective user id.
/// Real volumes, made from disposable disk images (`hdiutil`, no root;
/// review el-6d0i0, Director decision A): a target whose volume cannot keep
/// private folders private is refused before anything is written, and
/// volumes that record owners still take the move.
#[cfg(all(test, target_os = "macos"))]
mod volume_tests {
    use super::caller_tests::{aside_dirs, marker, plenty, Env};
    use super::*;
    use std::collections::BTreeMap;
    use std::process::Command;

    static HDIUTIL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Image {
        _dir: tempfile::TempDir,
        mount: PathBuf,
    }

    impl Image {
        /// `owners`: `None` attaches like a double click would (external
        /// volumes: ownership ignored), `Some(true)` with `-owners on`.
        fn new(fs: &str, owners: Option<bool>) -> Self {
            // Concurrent `hdiutil create` calls were seen to hang for good.
            let _one = HDIUTIL.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let image = dir.path().join("volume.dmg");
            let mount = dir.path().join("mnt");
            run(Command::new("hdiutil")
                .args(["create", "-quiet", "-size", "64m", "-fs", fs])
                .args(["-volname", "PCTEST"])
                .arg(&image));
            let mut attach = Command::new("hdiutil");
            attach.args(["attach", "-quiet", "-nobrowse", "-noverify"]);
            if let Some(on) = owners {
                attach.args(["-owners", if on { "on" } else { "off" }]);
            }
            run(attach.arg("-mountpoint").arg(&mount).arg(&image));
            let mount = mount.canonicalize().unwrap();
            Self { _dir: dir, mount }
        }
    }

    impl Drop for Image {
        fn drop(&mut self) {
            let _one = HDIUTIL.lock().unwrap_or_else(|e| e.into_inner());
            let _ = Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.mount)
                .status();
        }
    }

    fn run(command: &mut Command) {
        let out = command.output().unwrap();
        assert!(out.status.success(), "{command:?}: {out:?}");
    }

    /// Every entry under `dir` with its contents (files) or `None`.
    fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
        let mut all = BTreeMap::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for entry in fs::read_dir(&d).unwrap() {
                let path = entry.unwrap().path();
                let meta = fs::symlink_metadata(&path).unwrap();
                if meta.is_dir() {
                    todo.push(path.clone());
                    all.insert(path, None);
                } else {
                    all.insert(path.clone(), Some(fs::read(&path).unwrap()));
                }
            }
        }
        all
    }

    fn refusal<'a>(blockers: &'a [Blocker], target: &Path) -> &'a str {
        blockers
            .iter()
            .find_map(|b| match b {
                Blocker::NoPrivateFolders { path, reason } if path == target => {
                    Some(reason.as_str())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no refusal for {}: {blockers:?}", target.display()))
    }

    /// FAT16/FAT32/exFAT never record owners; HFS+ and APFS attached like
    /// an external disk ignore them. For an existing folder with someone's
    /// photo in it, and for one still to be created: the preview names the
    /// target and the volume, the move is refused with the same blocker,
    /// and nothing at all is written on the volume — no writer lock, no
    /// reservation, no staging — so a retry meets exactly the same state.
    /// The source, its data and the bootstrap stay as they were.
    #[test]
    fn volumes_that_ignore_owners_are_refused_before_anything_is_written() {
        for (fs_name, owners, shown) in [
            ("MS-DOS", None, "msdos"),
            ("MS-DOS FAT32", Some(true), "msdos"),
            ("ExFAT", None, "exfat"),
            ("HFS+", None, "hfs"),
            ("APFS", None, "apfs"),
        ] {
            let image = Image::new(fs_name, owners);
            let env = Env::new();
            let existing = image.mount.join("data");
            fs::create_dir(&existing).unwrap();
            fs::write(existing.join("foreign-photo.jpg"), b"\xff\xd8 foreign").unwrap();
            let missing = image.mount.join("new/data");
            let volume_before = tree(&image.mount);
            let bootstrap = env.bootstrap();
            for target in [&existing, &missing] {
                for attempt in 0..2 {
                    let preview =
                        preview_move_with(&env.dirs, &env.layout, Source::System, target, plenty);
                    let reason = refusal(&preview.blockers, target);
                    let shown_mount = format!("{} ({shown})", image.mount.display());
                    assert!(reason.contains(&shown_mount), "{fs_name}: {reason}");
                    assert!(
                        preview.reasons.iter().any(
                            |r| r.contains(&target.display().to_string()) && r.contains(reason)
                        ),
                        "{fs_name}: {:?}",
                        preview.reasons
                    );
                    let result = move_data(&env.dirs, &env.layout, Source::System, target, plenty);
                    let Err(RelocateError::Blocked { blockers }) = &result else {
                        panic!("{fs_name} {attempt}: {result:?}");
                    };
                    assert_eq!(refusal(blockers, target), reason);
                    assert_eq!(tree(&image.mount), volume_before, "{fs_name} {attempt}");
                }
            }
            assert!(!missing.parent().unwrap().exists());
            assert_eq!(env.bootstrap(), bootstrap);
            assert_eq!(env.chosen(), env.layout.dir);
            assert_eq!(marker(&env.layout.db), "исходная");
        }
    }

    /// The same HFS+ and APFS attached with ownership on keep the move
    /// working end to end, with no private folder left behind.
    #[test]
    fn volumes_that_record_owners_take_the_move() {
        for fs_name in ["HFS+", "APFS"] {
            let image = Image::new(fs_name, Some(true));
            let env = Env::new();
            let target = image.mount.join("data");
            let preview =
                preview_move_with(&env.dirs, &env.layout, Source::System, &target, plenty);
            assert!(
                preview.blockers.is_empty(),
                "{fs_name}: {:?}",
                preview.reasons
            );
            move_data(&env.dirs, &env.layout, Source::System, &target, plenty)
                .unwrap_or_else(|e| panic!("{fs_name}: {e}"));
            assert_eq!(env.chosen(), target);
            assert_eq!(marker(&target.join(DB_FILE)), "исходная");
            assert!(!target.join(PARTIAL_DB).exists());
            assert!(!target.join(PARTIAL_THUMBS).exists());
            assert!(aside_dirs(&target).is_empty());
        }
    }

    /// el-21zyg, observed natively: exFAT fails `renameatx_np(RENAME_EXCL)`
    /// with `ENOTSUP` (45) and reports `VOL_CAP_INT_RENAME_EXCL` absent;
    /// APFS and HFS+ report it. The preview names that for exFAT next to
    /// the ownership refusal, and the move is refused with both before
    /// anything is written, every time.
    #[test]
    fn exfat_cannot_rename_without_replacing_and_is_refused_for_it() {
        let exfat = Image::new("ExFAT", None);
        let (from, to) = (exfat.mount.join("a"), exfat.mount.join("b"));
        fs::write(&from, b"synthetic").unwrap();
        let error = pc_core::disk::rename_no_replace(&from, &to).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOTSUP), "{error}");
        assert!(from.exists() && !to.exists());
        fs::remove_file(&from).unwrap();

        let reason = volume::check_exclusive_rename(&exfat.mount.join("data")).unwrap_err();
        assert!(reason.contains("(exfat)"), "{reason}");
        let env = Env::new();
        let target = exfat.mount.join("data");
        let preview = preview_move_with(&env.dirs, &env.layout, Source::System, &target, plenty);
        assert!(preview.blockers.iter().any(|b| matches!(b,
            Blocker::NoExclusiveRename { path, reason: r } if *path == target && *r == reason)));
        assert!(preview
            .blockers
            .iter()
            .any(|b| matches!(b, Blocker::NoPrivateFolders { .. })));
        let volume_before = tree(&exfat.mount);
        for _ in 0..2 {
            let result = move_data(&env.dirs, &env.layout, Source::System, &target, plenty);
            let Err(RelocateError::Blocked { blockers }) = &result else {
                panic!("{result:?}");
            };
            assert_eq!(blockers, &preview.blockers);
            assert_eq!(tree(&exfat.mount), volume_before);
        }
        assert_eq!(marker(&env.layout.db), "исходная");

        for fs_name in ["APFS", "HFS+"] {
            let image = Image::new(fs_name, Some(true));
            assert_eq!(
                volume::check_exclusive_rename(&image.mount.join("new/data")),
                Ok(()),
                "{fs_name}"
            );
        }
    }

    /// If the volume changes between the preview and the first write (or a
    /// caller skips the preview), the folder just made is checked through
    /// its descriptor: on a noowners volume it is refused although it
    /// "belongs" to us and has mode 0700 — it was made by this run, so it
    /// is left in place and named, not removed. Where the private folder
    /// cannot even be created (FAT: no ACLs), the error names the path.
    #[test]
    fn a_folder_made_on_a_volume_without_owners_is_not_registered() {
        let apfs = Image::new("APFS", None);
        let path = apfs.mount.join(PARTIAL_DB);
        let error = OwnedDirectory::create(&path).err().expect("refused");
        let text = error.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("noowners"), "{text}");
        assert!(text.contains("nothing was removed"), "{text}");
        assert!(path.is_dir());

        let fat = Image::new("MS-DOS", None);
        let path = fat.mount.join(PARTIAL_DB);
        let error = OwnedDirectory::create(&path).err().expect("refused");
        let text = error.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("private folder"), "{text}");
        assert!(!path.exists());
    }
}

#[cfg(all(test, unix))]
mod registration_tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// `create_with` with `swap` run between `mkdir` and the identity.
    fn create_swapped(path: &Path, swap: impl FnOnce(&Path)) -> io::Result<OwnedDirectory> {
        let mut swap = Some(swap);
        OwnedDirectory::create_with(path, &mut |step, at| {
            if step == Step::Created {
                (swap.take().unwrap())(at);
            }
        })
    }

    fn assert_refused(result: io::Result<OwnedDirectory>, path: &Path) {
        let Err(e) = result else {
            panic!("a swapped-in entry was registered as ours");
        };
        let text = e.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("nothing was removed"), "{text}");
    }

    fn no_private_dirs(dir: &Path) {
        for e in fs::read_dir(dir).unwrap() {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!name.starts_with(ASIDE_PREFIX), "left {name}");
        }
    }

    /// The exact reproducer from el-3xm8: our empty folder is renamed away
    /// and a folder with a photo takes its name before the identity is
    /// taken. Both must survive, for either mode of the foreign folder.
    #[test]
    fn a_foreign_directory_swapped_before_identity_capture_survives() {
        for mode in [0o755, 0o700] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(PARTIAL_DB);
            let own = temp.path().join("moved-own-dir");
            let result = create_swapped(&path, |p| {
                fs::rename(p, &own).unwrap();
                fs::create_dir(p).unwrap();
                fs::write(p.join("foreign-photo.jpg"), b"\xff\xd8 foreign payload").unwrap();
                set_mode(p, mode);
            });
            assert_refused(result, &path);
            assert_eq!(
                fs::read(path.join("foreign-photo.jpg")).unwrap(),
                b"\xff\xd8 foreign payload"
            );
            assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, mode);
            // Our own directory is not ours to prove any more: kept too.
            assert!(own.is_dir());
            no_private_dirs(temp.path());
        }
    }

    /// Links (to a foreign folder, to our own moved folder, dangling) and
    /// files swapped onto the name are refused without being followed,
    /// and stay exactly as they were.
    #[test]
    fn swapped_in_links_and_files_are_refused_and_kept() {
        #[derive(Debug, Clone, Copy)]
        enum Swap {
            LinkToForeignDir,
            LinkToOwnDir,
            DanglingLink,
            File,
            EmptyFile,
        }
        for swap in [
            Swap::LinkToForeignDir,
            Swap::LinkToOwnDir,
            Swap::DanglingLink,
            Swap::File,
            Swap::EmptyFile,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(PARTIAL_THUMBS);
            let own = temp.path().join("moved-own-dir");
            let foreign = temp.path().join("photos-2015");
            fs::create_dir(&foreign).unwrap();
            fs::write(foreign.join("IMG_0001.JPG"), b"only copy").unwrap();
            set_mode(&foreign, 0o700);
            let result = create_swapped(&path, |p| {
                fs::rename(p, &own).unwrap();
                match swap {
                    Swap::LinkToForeignDir => symlink(&foreign, p).unwrap(),
                    Swap::LinkToOwnDir => symlink(&own, p).unwrap(),
                    Swap::DanglingLink => symlink(temp.path().join("nowhere"), p).unwrap(),
                    Swap::File => fs::write(p, b"foreign file").unwrap(),
                    Swap::EmptyFile => fs::write(p, b"").unwrap(),
                }
            });
            assert_refused(result, &path);
            let meta = fs::symlink_metadata(&path).unwrap();
            match swap {
                Swap::LinkToForeignDir => assert_eq!(fs::read_link(&path).unwrap(), foreign),
                Swap::LinkToOwnDir => assert_eq!(fs::read_link(&path).unwrap(), own),
                Swap::DanglingLink => assert!(meta.file_type().is_symlink()),
                Swap::File => assert_eq!(fs::read(&path).unwrap(), b"foreign file"),
                Swap::EmptyFile => assert!(meta.is_file() && meta.len() == 0),
            }
            assert_eq!(
                fs::read(foreign.join("IMG_0001.JPG")).unwrap(),
                b"only copy"
            );
            assert!(own.is_dir(), "{swap:?}");
            no_private_dirs(temp.path());
        }
    }

    /// Metadata of the swapped-in directory: group/other bits refuse it
    /// even when empty. An empty 0700 directory of the same user cannot be
    /// told from ours — a same-user actor is outside the threat model; this
    /// pins that limit: it is registered, and only it (with what this run
    /// puts in it) is removed on release, never a neighbour.
    #[test]
    fn metadata_decides_for_empty_swaps_and_the_same_user_limit_is_pinned() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let own = temp.path().join("moved-own-dir");
        let result = create_swapped(&path, |p| {
            fs::rename(p, &own).unwrap();
            fs::create_dir(p).unwrap();
            set_mode(p, 0o750);
        });
        assert_refused(result, &path);
        assert!(path.is_dir() && own.is_dir());

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let own = temp.path().join("moved-own-dir");
        let owned = create_swapped(&path, |p| {
            fs::rename(p, &own).unwrap();
            fs::create_dir(p).unwrap();
            set_mode(p, 0o700);
        })
        .unwrap();
        fs::write(path.join(DB_FILE), b"this run's copy").unwrap();
        owned.release().unwrap();
        assert!(!path.exists());
        assert!(own.is_dir(), "the neighbour was touched");
    }

    /// The owner check, through the descriptor: a directory that belongs to
    /// someone else (here: a different effective user id) is refused.
    #[test]
    fn a_directory_of_another_user_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("d");
        fs::create_dir(&dir).unwrap();
        set_mode(&dir, 0o700);
        let file = fs::File::open(&dir).unwrap();
        let uid = fs::metadata(&dir).unwrap().uid();
        assert_eq!(fresh_private_dir(&file, uid), Ok(()));
        let why = fresh_private_dir(&file, uid.wrapping_add(1)).unwrap_err();
        assert!(why.contains(&format!("belongs to user {uid}")), "{why}");

        // Emptiness is read through the same descriptor, including an entry
        // added after it was opened, and a hidden one.
        fs::write(dir.join(".DS_Store"), b"").unwrap();
        assert_eq!(
            fresh_private_dir(&file, uid).unwrap_err(),
            "it is not empty"
        );
        assert!(dir.join(".DS_Store").exists());
    }

    /// Without a race: the directory is made private by `mkdir` itself,
    /// registered, and removed again with its contents.
    #[test]
    fn an_undisturbed_directory_is_private_registered_and_released() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.is_dir());
        assert_eq!(meta.mode() & 0o077, 0, "{:o}", meta.mode());
        fs::create_dir(path.join("sub")).unwrap();
        fs::write(path.join("sub/thumb.jpg"), b"thumb").unwrap();
        owned.release().unwrap();
        assert!(!path.exists());
        no_private_dirs(temp.path());
    }

    /// An entry that is already at the name is never registered or changed:
    /// `mkdir` fails first.
    #[test]
    fn a_pre_existing_entry_is_left_alone() {
        let temp = tempfile::tempdir().unwrap();
        let foreign = temp.path().join("photos");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("a.jpg"), b"a").unwrap();
        for (i, plant) in [
            &(|p: &Path| fs::create_dir(p).unwrap()) as &dyn Fn(&Path),
            &|p: &Path| fs::write(p, b"file").unwrap(),
            &|p: &Path| symlink(&foreign, p).unwrap(),
        ]
        .into_iter()
        .enumerate()
        {
            let path = temp.path().join(format!("{i}{PARTIAL_DB}"));
            plant(&path);
            let before = fs::symlink_metadata(&path).unwrap();
            let e = OwnedDirectory::create(&path).err().expect("registered");
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
            let after = fs::symlink_metadata(&path).unwrap();
            assert_eq!((before.ino(), before.mode()), (after.ino(), after.mode()));
        }
        assert_eq!(fs::read(foreign.join("a.jpg")).unwrap(), b"a");
    }
}

/// macOS extended ACLs at the registration boundary (review el-1wh7b): a
/// parent with an inheritable `everyone` ACL opened every 0700 folder made
/// in it to everybody, and such a folder was registered as private.
/// Review el-5null: the private cleanup folder's *name* lives in the shared
/// parent. Anyone who may rename entries there (another account with
/// `delete_child`/`add_subdirectory`, or write on a non-sticky parent) can
/// move the whole private folder away after our entry was proven ours in
/// it and put a foreign tree under the old names — without ever looking
/// inside the private folder. A cleanup that deletes by path then deletes
/// the foreign tree. These hooks do exactly that between the proof and the
/// delete (and at the other steps), then check that nothing foreign is
/// touched and that the caller is told where things are.
#[cfg(all(test, unix))]
mod aside_namespace_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    const PAYLOAD: &[u8] = b"unique foreign payload 92741";

    /// Bytes, mode, modification time (s, ns).
    type Fingerprint = (Vec<u8>, u32, i64, i64);

    /// Bytes, mode and modification time: what "preserved" means here.
    fn fingerprint(path: &Path) -> Fingerprint {
        let m = fs::symlink_metadata(path).unwrap();
        (fs::read(path).unwrap(), m.mode(), m.mtime(), m.mtime_nsec())
    }

    /// A foreign tree `<aside>/<name>/foreign-photo.jpg` (or, for a file
    /// entry, `<aside>/<name>` itself) with a distinctive mode and time.
    fn plant_foreign(at: &Path, directory: bool) -> PathBuf {
        let photo = if directory {
            fs::create_dir(at).unwrap();
            at.join("foreign-photo.jpg")
        } else {
            at.to_path_buf()
        };
        fs::write(&photo, PAYLOAD).unwrap();
        fs::set_permissions(&photo, fs::Permissions::from_mode(0o640)).unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::options()
            .write(true)
            .open(&photo)
            .unwrap()
            .set_modified(old)
            .unwrap();
        photo
    }

    /// The hook of the review: at `step`, rename the private folder that
    /// holds `at` to `saved`, recreate its name and plant a foreign entry
    /// under our entry's name.
    fn swap_private_folder<'a>(
        step: Step,
        directory: bool,
        saved: &'a Path,
        planted: &'a mut Option<(PathBuf, Fingerprint)>,
    ) -> impl FnMut(Step, &Path) + 'a {
        move |now, at| {
            if now == step && planted.is_none() {
                let aside = at.parent().unwrap();
                fs::rename(aside, saved).unwrap();
                fs::create_dir(aside).unwrap();
                let photo = plant_foreign(at, directory);
                *planted = Some((photo.clone(), fingerprint(&photo)));
            }
        }
    }

    /// Directory staging, swap right before the delete (the review's case)
    /// and right after the move (before the proof).
    #[test]
    fn a_renamed_private_folder_never_takes_a_foreign_tree_with_it() {
        for step in [Step::BeforeDelete, Step::Moved] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("staging");
            let owned = OwnedDirectory::create(&path).unwrap();
            fs::write(path.join("own-copy"), b"ours").unwrap();
            let saved = temp.path().join("saved-aside");
            let mut planted = None;
            let result =
                owned.release_with(&mut swap_private_folder(step, true, &saved, &mut planted));
            let (photo, before) = planted.expect("the hook ran");
            assert_eq!(fingerprint(&photo), before, "{step:?}: {result:?}");
            let aside = photo.parent().unwrap().parent().unwrap();
            let error = result.expect_err("a moved private folder is reported");
            assert!(error.contains(&aside.display().to_string()), "{error}");
            assert!(error.contains("moved or replaced"), "{error}");
            // Our own copy is gone from wherever the private folder went;
            // the folder itself, now empty, is reported where it is.
            assert_eq!(fs::read_dir(&saved).unwrap().count(), 0, "{step:?}");
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            assert!(
                error.contains(&saved.canonicalize().unwrap().display().to_string()),
                "{error}"
            );
            assert!(!path.exists());
        }
    }

    /// The same for the empty sidecar reservations.
    #[test]
    fn a_renamed_private_folder_never_takes_a_foreign_sidecar_with_it() {
        for step in [Step::BeforeDelete, Step::Moved] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join(DB_FILE);
            let mut reservations = SidecarReservations::default();
            reservations.claim(&db).unwrap();
            let saved = temp.path().join("saved-aside");
            let mut planted = None;
            let left = reservations.release_with(&mut swap_private_folder(
                step,
                false,
                &saved,
                &mut planted,
            ));
            let (photo, before) = planted.expect("the hook ran");
            assert_eq!(fingerprint(&photo), before, "{step:?}: {left:?}");
            assert_eq!(left.len(), 1, "{left:?}");
            let aside = photo.parent().unwrap();
            assert!(left[0].contains(&aside.display().to_string()), "{left:?}");
            assert_eq!(fs::read_dir(&saved).unwrap().count(), 0, "{step:?}");
            for suffix in SIDECARS {
                assert!(!sidecar(&db, suffix).exists(), "{suffix}");
            }
        }
    }

    /// The parent itself is renamed and replaced by a folder with a foreign
    /// entry under our name, right before our entry is moved aside: the
    /// cleanup keeps working in the folder it checked, and the new one is
    /// not touched.
    #[test]
    fn a_replaced_parent_is_not_cleaned_in_our_place() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("target");
        fs::create_dir(&parent).unwrap();
        let path = parent.join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        fs::write(path.join(DB_FILE), b"ours").unwrap();
        let saved = temp.path().join("saved-target");
        let mut planted = None;
        owned
            .release_with(&mut |step, at| {
                if step == Step::BeforeMove {
                    fs::rename(&parent, &saved).unwrap();
                    fs::create_dir(&parent).unwrap();
                    let photo = plant_foreign(at, true);
                    planted = Some((photo.clone(), fingerprint(&photo)));
                }
            })
            .unwrap();
        let (photo, before) = planted.unwrap();
        assert_eq!(fingerprint(&photo), before);
        assert_eq!(fs::read_dir(&saved).unwrap().count(), 0);
    }

    /// Through the caller: the swap after the proof ends the move with
    /// `Incomplete` naming the private folder, the bootstrap is unchanged
    /// and the foreign tree is intact.
    #[test]
    fn the_caller_is_told_and_the_foreign_tree_survives() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let dirs = SystemDirs {
            app_local_data: root.join("local/app"),
            exe_dir: None,
            portable_supported: false,
        };
        let r = crate::resolve::resolve(&dirs, None).unwrap();
        let prepared = crate::resolve::prepare(&dirs, &r).unwrap();
        crate::resolve::confirm_started(&dirs, &prepared).unwrap();
        let before = fs::read(dirs.bootstrap_path()).ok();
        let target = root.join("target");
        let saved = root.join("saved-aside");
        let mut planted = None;
        let result = move_data_with(
            &dirs,
            &prepared.layout,
            Source::System,
            &target,
            |_| Ok(1 << 40),
            &mut swap_private_folder(Step::BeforeDelete, true, &saved, &mut planted),
        );
        let (photo, fp) = planted.expect("the hook ran");
        assert_eq!(fingerprint(&photo), fp);
        let Err(RelocateError::Incomplete { copy, left }) = &result else {
            panic!("the shell would restart on {result:?}");
        };
        assert_eq!(copy, &target);
        let aside = photo.parent().unwrap().parent().unwrap();
        assert!(left.contains(&aside.display().to_string()), "{left}");
        assert_eq!(fs::read(dirs.bootstrap_path()).ok(), before);
        assert!(target.join(DB_FILE).is_file());
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_acl_tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;
    use std::process::Command;

    const OPEN_TO_EVERYONE: &str = "everyone allow list,search,add_file,add_subdirectory,\
        delete_child,file_inherit,directory_inherit";

    fn add_acl(path: &Path, entry: &str) {
        let out = Command::new("/bin/chmod")
            .args(["+a", entry])
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }

    /// `ls -led` lines listing ACL entries (" 0: ...").
    fn acl_lines(path: &Path) -> Vec<String> {
        let out = Command::new("/bin/ls")
            .arg("-led")
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect()
    }

    fn open_parent(temp: &tempfile::TempDir) -> PathBuf {
        let parent = temp.path().join("shared");
        fs::create_dir(&parent).unwrap();
        add_acl(&parent, OPEN_TO_EVERYONE);
        // The premise of the test: plain `mkdir` 0700 inherits it.
        let probe = parent.join("probe");
        fs::DirBuilder::new().mode(0o700).create(&probe).unwrap();
        assert!(
            acl_lines(&probe)
                .iter()
                .any(|l| l.contains("inherited allow")),
            "{:?}",
            acl_lines(&probe)
        );
        fs::remove_dir(&probe).unwrap();
        parent
    }

    /// The reviewer's scenario, end to end: the staging directory and the
    /// private cleanup folder are both created without the parent's ACL,
    /// and the run's own content is removed on release.
    #[test]
    fn folders_made_under_an_inheritable_acl_do_not_inherit_it() {
        let temp = tempfile::tempdir().unwrap();
        let parent = open_parent(&temp);
        let path = parent.join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        assert_eq!(acl_lines(&path), Vec::<String>::new());
        let file = fs::File::open(&path).unwrap();
        assert_eq!(macos_acl::no_extended_acl(&file), Ok(()));
        fs::write(path.join(DB_FILE), b"this run's copy").unwrap();

        let mut asides = Vec::new();
        owned
            .release_with(&mut |step, at| {
                if step == Step::AsideCreated {
                    assert_eq!(acl_lines(at), Vec::<String>::new());
                    asides.push(at.to_path_buf());
                }
            })
            .unwrap();
        assert_eq!(asides.len(), 1);
        assert!(!path.exists() && !asides[0].exists());
    }

    /// A folder of this user, mode 0700, empty — but with the parent's ACL,
    /// as a plain `mkdir` makes it — swapped onto the name is refused and
    /// kept; so is our own folder, wherever it went.
    #[test]
    fn a_swapped_in_folder_with_an_inherited_acl_is_refused_and_kept() {
        let temp = tempfile::tempdir().unwrap();
        let parent = open_parent(&temp);
        let path = parent.join(PARTIAL_THUMBS);
        let own = parent.join("moved-own-dir");
        let result = OwnedDirectory::create_with(&path, &mut |step, at| {
            if step == Step::Created {
                fs::rename(at, &own).unwrap();
                fs::DirBuilder::new().mode(0o700).create(at).unwrap();
            }
        });
        let e = result
            .err()
            .expect("a folder open to everyone was registered");
        let text = e.to_string();
        assert!(text.contains("access list"), "{text}");
        assert!(text.contains("nothing was removed"), "{text}");
        assert!(path.is_dir() && own.is_dir());
        assert!(!acl_lines(&path).is_empty());
    }

    /// An ACL on the private cleanup folder (here: added right after it was
    /// made) stops the cleanup before anything is moved: the staging
    /// directory and its content stay at their name, the folder stays too,
    /// and the error names both.
    #[test]
    fn a_private_folder_with_an_acl_stops_cleanup_and_nothing_is_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        fs::write(path.join(DB_FILE), b"this run's copy").unwrap();
        let mut aside = None;
        let e = owned
            .release_with(&mut |step, at| {
                if step == Step::AsideCreated {
                    add_acl(at, "everyone allow add_file,add_subdirectory,delete_child");
                    aside = Some(at.to_path_buf());
                }
            })
            .unwrap_err();
        let aside = aside.unwrap();
        assert!(e.contains(&path.display().to_string()), "{e}");
        assert!(e.contains(&aside.display().to_string()), "{e}");
        assert!(e.contains("left in place"), "{e}");
        assert_eq!(fs::read(path.join(DB_FILE)).unwrap(), b"this run's copy");
        assert!(aside.is_dir());
        assert_eq!(fs::read_dir(&aside).unwrap().count(), 0);
    }

    /// Review el-5null, verbatim scenario: a parent open to everyone
    /// (`delete_child`, `add_subdirectory`), so another account may rename
    /// the private folder itself. After our entry is proven ours in it, the
    /// whole private folder is renamed to a sibling and a foreign tree put
    /// under the old names — nothing inside the private folder is touched.
    /// Before the fix `remove_dir_all` of the old path deleted
    /// `foreign-photo.jpg` and returned `Ok` (0/1). Now the foreign tree is
    /// intact, our own copy is gone from the moved folder, and the error
    /// names the old name and where the private folder went.
    #[test]
    fn a_private_folder_renamed_in_an_open_parent_keeps_the_foreign_photo() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let parent = open_parent(&temp);
        for staging in [PARTIAL_DB, PARTIAL_THUMBS] {
            let path = parent.join(staging);
            let owned = OwnedDirectory::create(&path).unwrap();
            fs::write(path.join("own-copy"), b"ours").unwrap();
            let saved = parent.join(format!("saved-aside-{staging}"));
            let mut foreign = None;
            let result = owned.release_with(&mut |step, at| {
                if step == Step::BeforeDelete {
                    let aside = at.parent().unwrap();
                    fs::rename(aside, &saved).unwrap();
                    fs::create_dir(aside).unwrap();
                    fs::create_dir(at).unwrap();
                    let photo = at.join("foreign-photo.jpg");
                    fs::write(&photo, b"unique foreign payload 92741").unwrap();
                    foreign = Some(photo);
                }
            });
            let photo = foreign.unwrap();
            assert_eq!(
                fs::read(&photo).unwrap(),
                b"unique foreign payload 92741",
                "{result:?}"
            );
            let error = result.unwrap_err();
            let aside = photo.parent().unwrap().parent().unwrap();
            assert!(error.contains(&aside.display().to_string()), "{error}");
            assert!(
                error.contains(&saved.canonicalize().unwrap().display().to_string()),
                "{error}"
            );
            assert!(!saved.join(staging).exists());
            assert!(!path.exists());
        }
    }

    /// The check itself, through the descriptor: any entry refuses, a
    /// folder without ACL passes.
    #[test]
    fn an_acl_entry_fails_the_private_check() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("d");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let file = fs::File::open(&dir).unwrap();
        // SAFETY: no preconditions.
        let euid = unsafe { libc::geteuid() };
        assert_eq!(fresh_private_dir(&file, euid), Ok(()));
        add_acl(&dir, "everyone deny delete");
        let why = fresh_private_dir(&file, euid).unwrap_err();
        assert!(why.contains("access list"), "{why}");
    }
}

/// Windows registration boundary (el-2xri, review el-1wh7b). Written for
/// and type-checked against the Windows target; see DESKTOP.md for where
/// they have (not) been run.
#[cfg(all(test, windows))]
mod windows_registration_tests {
    use super::*;
    use std::process::Command;

    fn create_swapped(path: &Path, swap: impl FnOnce(&Path)) -> io::Result<OwnedDirectory> {
        let mut swap = Some(swap);
        OwnedDirectory::create_with(path, &mut |step, at| {
            if step == Step::Created {
                (swap.take().unwrap())(at);
            }
        })
    }

    fn refusal(result: io::Result<OwnedDirectory>, path: &Path) -> String {
        let Err(e) = result else {
            panic!("a swapped-in entry was registered as ours");
        };
        let text = e.to_string();
        assert!(text.contains(&path.display().to_string()), "{text}");
        assert!(text.contains("nothing was removed"), "{text}");
        text
    }

    fn user_sddl(extra: &str) -> String {
        let user = windows_acl::User::current().unwrap();
        let sid = user.sid_string().unwrap();
        format!("O:{sid}D:P(A;OICI;FA;;;{sid}){extra}")
    }

    #[test]
    fn a_new_private_directory_passes_and_is_released() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let owned = OwnedDirectory::create(&path).unwrap();
        open_created_dir(&path).unwrap();
        fs::create_dir(path.join("sub")).unwrap();
        fs::write(path.join("sub").join("thumb.jpg"), b"thumb").unwrap();
        owned.release().unwrap();
        assert!(!path.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    /// The el-3xm8 reproducer on Windows: a folder with default (inherited)
    /// permissions, with or without a photo, takes the name.
    #[test]
    fn a_folder_with_inherited_permissions_is_refused_and_kept() {
        for with_photo in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join(PARTIAL_DB);
            let own = temp.path().join("moved-own-dir");
            let text = refusal(
                create_swapped(&path, |p| {
                    fs::rename(p, &own).unwrap();
                    fs::create_dir(p).unwrap();
                    if with_photo {
                        fs::write(p.join("foreign-photo.jpg"), b"\xff\xd8 foreign").unwrap();
                    }
                }),
                &path,
            );
            assert!(text.contains("inherited"), "{text}");
            if with_photo {
                assert_eq!(
                    fs::read(path.join("foreign-photo.jpg")).unwrap(),
                    b"\xff\xd8 foreign"
                );
            }
            assert!(own.is_dir());
        }
    }

    #[test]
    fn a_private_folder_that_is_not_empty_or_open_to_others_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(PARTIAL_DB);
        let own = temp.path().join("moved-own-dir");
        let text = refusal(
            create_swapped(&path, |p| {
                fs::rename(p, &own).unwrap();
                windows_acl::mkdir_private(p).unwrap();
                fs::write(p.join("foreign-photo.jpg"), b"foreign").unwrap();
            }),
            &path,
        );
        assert!(text.contains("not empty"), "{text}");
        assert_eq!(
            fs::read(path.join("foreign-photo.jpg")).unwrap(),
            b"foreign"
        );

        let path = temp.path().join(PARTIAL_THUMBS);
        let own = temp.path().join("moved-own-dir-2");
        let text = refusal(
            create_swapped(&path, |p| {
                fs::rename(p, &own).unwrap();
                // Everyone (WD) may write.
                windows_acl::Descriptor::from_sddl(&user_sddl("(A;OICI;FA;;;WD)"))
                    .unwrap()
                    .mkdir(p)
                    .unwrap();
            }),
            &path,
        );
        assert!(text.contains("other accounts"), "{text}");
        assert!(path.is_dir() && own.is_dir());
    }

    #[test]
    fn files_and_junctions_swapped_in_are_refused_and_kept() {
        let temp = tempfile::tempdir().unwrap();
        let foreign = temp.path().join("photos-2015");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("IMG_0001.JPG"), b"only copy").unwrap();

        let path = temp.path().join("file");
        refusal(
            create_swapped(&path, |p| {
                fs::rename(p, temp.path().join("own-1")).unwrap();
                fs::write(p, b"foreign file").unwrap();
            }),
            &path,
        );
        assert_eq!(fs::read(&path).unwrap(), b"foreign file");

        let path = temp.path().join("junction");
        refusal(
            create_swapped(&path, |p| {
                fs::rename(p, temp.path().join("own-2")).unwrap();
                let out = Command::new("cmd")
                    .args(["/C", "mklink", "/J"])
                    .arg(p)
                    .arg(&foreign)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{out:?}");
            }),
            &path,
        );
        assert!(fs::symlink_metadata(&path).is_ok());
        assert_eq!(
            fs::read(foreign.join("IMG_0001.JPG")).unwrap(),
            b"only copy"
        );
    }

    /// The owner check on a real folder owned by another account: the
    /// Windows directory (TrustedInstaller). Opened read-only, not changed.
    #[test]
    fn a_folder_of_another_account_is_refused() {
        let root = std::env::var_os("SystemRoot").expect("SystemRoot");
        let e = open_created_dir(Path::new(&root)).unwrap_err();
        assert!(e.to_string().contains("another account"), "{e}");
    }

    /// A tree with a read-only file and a junction to a foreign folder in
    /// it: deleted through handles, the junction as itself — the folder it
    /// points to keeps its photo.
    #[test]
    fn a_tree_is_deleted_through_handles_without_following_junctions() {
        let temp = tempfile::tempdir().unwrap();
        let foreign = temp.path().join("photos-2015");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("IMG_0001.JPG"), b"only copy").unwrap();
        let path = temp.path().join(PARTIAL_THUMBS);
        let owned = OwnedDirectory::create(&path).unwrap();
        fs::create_dir_all(path.join("ab").join("cd")).unwrap();
        let locked = path.join("ab").join("cd").join("thumb.jpg");
        fs::write(&locked, b"thumb").unwrap();
        let mut perms = fs::metadata(&locked).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&locked, perms).unwrap();
        let out = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(path.join("ab").join("link"))
            .arg(&foreign)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        owned.release().unwrap();
        assert!(!path.exists());
        assert_eq!(
            fs::read(foreign.join("IMG_0001.JPG")).unwrap(),
            b"only copy"
        );
    }
}
