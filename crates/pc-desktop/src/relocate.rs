//! Moving the app's own data — the database and the thumbnail cache — to
//! another folder. Never the photographs.
//!
//! The shell stops the server, calls [`copy_data`], then [`Copied::commit`] and
//! restarts; everything that decides whether that is safe lives here, free of
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
//!   [`remove_owned`] for the exact guarantee and its limits). What cannot
//!   be removed is reported, never silently left.

use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::bootstrap::{read_bootstrap, write_bootstrap, Bootstrap, Choice};
use crate::error::StartupError;
use crate::resolve::{choose_data_dir, NewDir, Source};
use crate::{DataLayout, SystemDirs, DB_FILE, THUMBS_DIR};

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
    /// The data folder is fixed for this launch (portable marker or
    /// `--data-dir`); the bootstrap would not be read.
    ModeFixed {
        source: Source,
    },
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
pub fn preview_move(from: &DataLayout, source: Source, target: &Path) -> MovePreview {
    preview_move_with(from, source, target, pc_core::disk::available_space)
}

/// [`preview_move`] with the free-space probe handed in, for tests.
pub fn preview_move_with(
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
    #[serde(skip)]
    sidecars: SidecarReservations,
    pub layout: DataLayout,
    pub tables: usize,
    pub rows: u64,
    pub thumbs_files: u64,
    pub thumbs_bytes: u64,
    /// The copy is published and proven, but the now-empty staging folder
    /// could not be removed; why, and where it is. It blocks a later move
    /// into the same folder until the user removes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staging_left: Option<String>,
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
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
) -> Result<Copied, RelocateError> {
    let preview = preview_move_with(from, source, target, &available);
    if !preview.blockers.is_empty() {
        return Err(RelocateError::Blocked {
            blockers: preview.blockers,
        });
    }
    let source_lock = pc_core::lock::take_writer(
        &from.db,
        &format!("moving the app data to {}", target.display()),
    )
    .map_err(|e| RelocateError::Locked {
        reason: format!("{e:#}"),
    })?;
    // Measured again under the lock: this is the size the copy must match.
    let size = measure(from).map_err(copy_err)?;
    let mut cleanup = Cleanup::new(target);
    let staged = match stage(from, source, target, &available, size, &mut cleanup) {
        Ok(staged) => staged,
        Err(error) => {
            // `stage` has released the target lock by now, so an empty
            // folder this run created can go too.
            let left = cleanup.abort();
            return Err(if left.is_empty() {
                error
            } else {
                RelocateError::Cleanup {
                    error: Box::new(error),
                    left,
                }
            });
        }
    };
    let staging_left = cleanup.succeed();
    let mut sidecars = cleanup.sidecars.take().unwrap_or_default();
    sidecars.keep = true;
    Ok(Copied {
        _source_lock: source_lock,
        _target_lock: staged.target_lock,
        sidecars,
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
    layout: DataLayout,
    tables: usize,
    rows: u64,
    files: u64,
    bytes: u64,
}

/// Everything `copy_data` does in the target. Whatever it creates is
/// recorded in `cleanup` as soon as it exists, so the caller can undo it.
fn stage(
    from: &DataLayout,
    source: Source,
    target: &Path,
    available: impl Fn(&Path) -> io::Result<u64>,
    size: DataSize,
    cleanup: &mut Cleanup,
) -> Result<Staged, RelocateError> {
    if !target.exists() {
        fs::create_dir_all(target).map_err(copy_err)?;
        cleanup.created_dir = true;
    }
    let to = DataLayout::in_dir(target);
    let target_lock = pc_core::lock::take_writer(&to.db, "receiving app data").map_err(|e| {
        RelocateError::Locked {
            reason: format!("{e:#}"),
        }
    })?;
    // Another process may have populated the target since the first preview.
    let preview = preview_move_with(from, source, target, &available);
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
    cleanup.db = Some(OwnedDirectory::create(&target.join(PARTIAL_DB)).map_err(copy_err)?);
    let partial_db = target.join(PARTIAL_DB).join(DB_FILE);
    let partial_thumbs = target.join(PARTIAL_THUMBS);
    reject_legacy_sidecars(target)?;
    let (tables, rows) = snapshot(&from.db, &partial_db)?;

    let (mut files, mut bytes) = (0, 0);
    if from.thumbs.exists() {
        cleanup.thumbs = Some(OwnedDirectory::create(&partial_thumbs).map_err(copy_err)?);
        copy_tree(&from.thumbs, &partial_thumbs).map_err(copy_err)?;
        (files, bytes) = tree_size(&partial_thumbs).map_err(copy_err)?;
        if (files, bytes) != (size.thumbs_files, size.thumbs_bytes) {
            return Err(RelocateError::Verify {
                reason: format!(
                    "thumbnails: {} files, {} bytes copied; {} files, {} bytes expected",
                    files, bytes, size.thumbs_files, size.thumbs_bytes
                ),
            });
        }
    }

    // The proven copies take their real names: thumbnails first, the
    // database last, so a target with `photo-cleanup.db` in it is always a
    // complete one.
    if let Some(sidecars) = &mut cleanup.sidecars {
        sidecars.ensure_owned().map_err(copy_err)?;
    }
    reject_legacy_sidecars(target)?;
    if let Some(thumbs) = &mut cleanup.thumbs {
        pc_core::disk::rename_no_replace(&partial_thumbs, &to.thumbs).map_err(copy_err)?;
        thumbs.path = to.thumbs.clone();
    }
    publish(&partial_db, &to.db).map_err(copy_err)?;
    sync_dir(target);
    Ok(Staged {
        target_lock,
        layout: to,
        tables,
        rows,
        files,
        bytes,
    })
}

/// Make the copy at `target` the data folder, keeping the current one as
/// `previous` until the next launch proves the new one starts.
impl Copied {
    /// Commit while both writer locks are still held. Dropping an uncommitted
    /// copy leaves the bootstrap unchanged and keeps the verified copy.
    pub fn commit(mut self, dirs: &SystemDirs, source: Source) -> Result<(), RelocateError> {
        self.sidecars.ensure_owned().map_err(copy_err)?;
        commit_move(dirs, source, &self.layout.dir)
    }
}

fn commit_move(dirs: &SystemDirs, source: Source, target: &Path) -> Result<(), RelocateError> {
    let current = current_choice(dirs, source)?;
    let next = if target == dirs.system_data_dir() {
        Choice::system()
    } else {
        Choice::custom(target.to_path_buf())
    };
    write_bootstrap(&dirs.bootstrap_path(), &Bootstrap::new(next, Some(current)))?;
    Ok(())
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

/// `VACUUM INTO` the partial file, then compare it with the source.
fn snapshot(src: &Path, partial: &Path) -> Result<(usize, u64), RelocateError> {
    let conn = Connection::open_with_flags(
        src,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(copy_err)?;
    conn.busy_timeout(std::time::Duration::from_secs(10))
        .map_err(copy_err)?;
    let target = partial.to_str().ok_or_else(|| RelocateError::Copy {
        reason: format!("the path is not valid Unicode: {}", partial.display()),
    })?;
    conn.execute("VACUUM INTO ?1", [target]).map_err(copy_err)?;
    fs::File::open(partial)
        .and_then(|f| f.sync_all())
        .map_err(copy_err)?;
    verify(&conn, partial).map_err(|reason| RelocateError::Verify { reason })
}

/// The copy opens, is intact, and holds what the source holds.
pub fn verify(src: &Connection, copy: &Path) -> Result<(usize, u64), String> {
    let dst = Connection::open_with_flags(copy, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
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
    let (a, b) = (schema(src)?, schema(&dst)?);
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
    if names != tables(&dst)? {
        return Err("the list of tables differs".into());
    }
    let mut rows = 0u64;
    for name in &names {
        // Names come from sqlite_master, quoted as identifiers.
        let q = format!("SELECT count(*) FROM \"{}\"", name.replace('"', "\"\""));
        let count = |c: &Connection| -> Result<i64, String> {
            c.query_row(&q, [], |r| r.get(0)).map_err(|e| e.to_string())
        };
        let (a, b) = (count(src)?, count(&dst)?);
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

/// Publish atomically without replacing a destination created during copying.
fn publish(partial: &Path, db: &Path) -> io::Result<()> {
    pc_core::disk::rename_no_replace(partial, db)
}

/// Removes what this run put in the target, unless the run succeeded.
///
/// [`Cleanup::abort`] reports what it could not remove; `Drop` is only the
/// last resort (a panic) and has nowhere to report to.
struct Cleanup {
    target: PathBuf,
    created_dir: bool,
    sidecars: Option<SidecarReservations>,
    db: Option<OwnedDirectory>,
    thumbs: Option<OwnedDirectory>,
}

impl Cleanup {
    fn new(target: &Path) -> Self {
        Self {
            target: target.to_path_buf(),
            created_dir: false,
            sidecars: None,
            db: None,
            thumbs: None,
        }
    }

    /// The copy is published: keep it, the thumbnails and the reservations;
    /// remove the database's now-empty staging folder. A failure there is
    /// returned, not fatal — the copy itself is complete.
    fn succeed(&mut self) -> Option<String> {
        if let Some(thumbs) = &mut self.thumbs {
            thumbs.keep = true;
        }
        self.thumbs = None;
        self.created_dir = false;
        self.db.take().and_then(|db| db.release().err())
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
        if std::mem::take(&mut self.created_dir) {
            // `rmdir` removes only an empty folder: one that someone put
            // something into meanwhile stays, and that is not an error.
            let _ = fs::remove_dir(&self.target);
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

fn reject_legacy_sidecars(target: &Path) -> Result<(), RelocateError> {
    let blockers: Vec<_> = SIDECARS
        .iter()
        .map(|suffix| sidecar(&target.join(PARTIAL_DB), suffix))
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
}

/// Points inside [`remove_owned_with`] where another process could act.
/// Tests act there; production passes [`no_race`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The public name was checked; it is about to be moved aside.
    BeforeMove,
    /// Moved aside, not yet checked there.
    Moved,
    /// Proven ours in the private folder; about to be deleted.
    BeforeDelete,
}

fn no_race(_: Step, _: &Path) {}

/// Remove `path` only if it is still the object behind `handle`.
///
/// POSIX has no "unlink this name only if it is still that inode", so a
/// check of `path` followed by a delete of `path` can delete whatever
/// another process renamed onto `path` in between. Instead:
///
/// 1. A replaced, changed or symlinked entry at `path` is left alone.
/// 2. A fresh folder with a unique name is created beside it (Unix mode
///    0700) and `path` is renamed into it. A rename moves exactly the entry
///    that is at `path` at that instant — never follows a symlink, never
///    replaces anything.
/// 3. The moved entry is checked again in the private folder. If it is not
///    ours (it was swapped in after step 1) it is renamed back without
///    replacement; if its name was taken again meanwhile it stays in the
///    private folder and the error says where. Nothing is deleted.
/// 4. Only an entry proven ours in the private folder is deleted.
///
/// Guaranteed: an entry that another process places at the public `path`
/// at any moment is never deleted. Not guaranteed, and documented as the
/// contract: (a) such an entry is briefly absent from `path` between steps
/// 2 and 3; (b) whatever another process moves into the unique private
/// folder, or into our own staging directory, is treated as ours; (c) bytes
/// written through a descriptor someone opened on our own reservation after
/// the step 3 check go with it. None of (b)/(c) can happen through the
/// public names the relocation shows to the world.
fn remove_owned(path: &Path, handle: &same_file::Handle, kind: Owned) -> Result<(), String> {
    remove_owned_with(path, handle, kind, &mut no_race)
}

fn remove_owned_with(
    path: &Path,
    handle: &same_file::Handle,
    kind: Owned,
    race: &mut dyn FnMut(Step, &Path),
) -> Result<(), String> {
    let kept = |why: &dyn fmt::Display| format!("{} was left in place: {why}", path.display());
    match fs::symlink_metadata(path) {
        // Gone already (SQLite consumes empty sidecars): nothing to do.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(kept(&e)),
        Ok(_) if !kind.is_ours(path, handle) => {
            return Err(kept(&"it was replaced or changed by someone else"))
        }
        Ok(_) => {}
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(kept(&"it has no parent folder"));
    };
    let aside = private_dir(parent).map_err(|e| kept(&e))?;
    let moved = aside.join(name);
    race(Step::BeforeMove, path);
    if let Err(e) = pc_core::disk::rename_no_replace(path, &moved) {
        let _ = fs::remove_dir(&aside);
        return match e.kind() {
            io::ErrorKind::NotFound => Ok(()),
            _ => Err(kept(&e)),
        };
    }
    race(Step::Moved, &moved);
    if !kind.is_ours(&moved, handle) {
        return match pc_core::disk::rename_no_replace(&moved, path) {
            Ok(()) => {
                let _ = fs::remove_dir(&aside);
                Err(kept(&"it was replaced by someone else"))
            }
            Err(e) => Err(format!(
                "{} replaced {} and was moved aside; it could not be put back ({e}). Nothing was deleted",
                moved.display(),
                path.display()
            )),
        };
    }
    race(Step::BeforeDelete, &moved);
    match kind {
        Owned::Directory => fs::remove_dir_all(&moved),
        Owned::EmptyFile => fs::remove_file(&moved),
    }
    .map_err(|e| {
        format!(
            "{} (this run's own {}) could not be removed: {e}",
            moved.display(),
            path.display()
        )
    })?;
    fs::remove_dir(&aside).map_err(|e| format!("{} could not be removed: {e}", aside.display()))
}

/// Prefix of the private folders cleanup moves entries into.
const ASIDE_PREFIX: &str = ".photo-cleanup-removing";

/// A new, empty folder in `parent` that nobody else has a name for.
fn private_dir(parent: &Path) -> io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    for _ in 0..16 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = parent.join(format!(
            "{ASIDE_PREFIX}-{}-{nanos:x}-{n}",
            std::process::id()
        ));
        match builder.create(&dir) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            r => return r.map(|()| dir),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for a private cleanup folder",
    ))
}

struct OwnedDirectory {
    path: PathBuf,
    handle: same_file::Handle,
    keep: bool,
}

impl OwnedDirectory {
    fn create(path: &Path) -> io::Result<Self> {
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(path)?;
        // On failure leave the directory rather than deleting an entry
        // whose identity we could not establish.
        Ok(Self {
            path: path.to_path_buf(),
            handle: same_file::Handle::from_path(path)?,
            keep: false,
        })
    }

    fn release(self) -> Result<(), String> {
        self.release_with(&mut no_race)
    }

    fn release_with(mut self, race: &mut dyn FnMut(Step, &Path)) -> Result<(), String> {
        self.keep = true;
        remove_owned_with(&self.path, &self.handle, Owned::Directory, race)
    }
}

impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = remove_owned(&self.path, &self.handle, Owned::Directory);
        }
    }
}

#[derive(Debug)]
struct EmptyReservation {
    path: PathBuf,
    handle: same_file::Handle,
}

impl EmptyReservation {
    fn claim(path: PathBuf) -> io::Result<Self> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
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

/// Regular files and folders only. The cache has nothing else; anything else
/// (a link that points outside) is a reason to stop, not to follow.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let dst = to.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&dst)?;
            copy_tree(&entry.path(), &dst)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &dst)?;
            fs::File::open(&dst)?.sync_all()?;
        } else {
            return Err(unexpected(&entry.path()));
        }
    }
    sync_dir(to);
    Ok(())
}

fn unexpected(p: &Path) -> io::Error {
    io::Error::other(format!("not a regular file or folder: {}", p.display()))
}

fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
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

    /// `copy_data`'s failure path returns what cleanup could not remove.
    #[test]
    fn abort_reports_a_replaced_staging_directory_and_keeps_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut cleanup = Cleanup::new(temp.path());
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
