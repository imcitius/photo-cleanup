//! Which data directory this launch uses, and getting it ready.
//!
//! Order, first match wins:
//!
//! 1. `--data-dir <path>` — for tests and advanced use; this launch only,
//!    never written to the bootstrap.
//! 2. Portable marker beside the executable (Windows only) →
//!    `<exe dir>/data`. If that cannot be written the launch fails; there is
//!    no quiet fallback to the user profile, which would split one archive
//!    into two without anyone noticing.
//! 3. The bootstrap → its `system` or `custom` directory, which must already
//!    hold a database.
//! 4. No bootstrap → first launch in `<app_local_data>/data`. The bootstrap
//!    is written only once the database there has opened.
//!
//! A new database is created only in cases 1, 2 and 4, or when the user
//! explicitly asks for one in a folder ([`choose_data_dir`] with
//! [`NewDir::CreateNew`]). The program directory is never written outside
//! explicit portable mode, so a launch from Program Files, a mounted disk
//! image or a translocated `.app` works.

use rusqlite::{Connection, OpenFlags, MAIN_DB};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::bootstrap::{read_bootstrap, write_bootstrap, Bootstrap, Choice, StoredMode};
use crate::error::{StartupError, Unavailable};
use crate::{DataLayout, SystemDirs, DATA_SUBDIR, DB_FILE};

/// Where this launch's data directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Override,
    Portable,
    System,
    Custom,
}

/// Whether the database may, must not, or must be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Creation {
    /// Open it if it is there, create it if not.
    MayCreate,
    /// It must be there. The directory is the user's archive; an empty
    /// database in its place would look like everything was lost.
    MustExist,
    /// It must not be there: the user asked for a new archive, and an
    /// existing one is never overwritten.
    MustBeNew,
}

/// What the user wants done with a folder they picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewDir {
    /// Switch to the archive already in it, without copying anything.
    UseExisting,
    /// Start a new, empty archive in it.
    CreateNew,
}

/// A decided but not yet prepared data directory. Nothing is written yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub source: Source,
    pub layout: DataLayout,
    pub creation: Creation,
    /// Written by [`prepare`], after the database opened.
    pub persist: Option<Bootstrap>,
    /// The choice before the last change, offered as "go back".
    pub previous: Option<Choice>,
    /// First launch only: a folder beside the executable that already holds
    /// a database — the batch-file distribution kept it there. The shell asks
    /// whether to use it ([`choose_data_dir`] with [`NewDir::UseExisting`])
    /// or start fresh; it is never picked up silently.
    pub legacy_dir: Option<PathBuf>,
}

/// A data directory whose database opened. Hand `layout` to `pc-api`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prepared {
    pub source: Source,
    pub layout: DataLayout,
    /// The database did not exist before this launch.
    pub created: bool,
}

/// Decide the data directory for this launch. Reads, never writes.
pub fn resolve(dirs: &SystemDirs, override_dir: Option<&Path>) -> Result<Resolved, StartupError> {
    if let Some(dir) = override_dir {
        require_absolute(dir)?;
        return Ok(fresh(Source::Override, dir, None));
    }
    if let Some(marker) = dirs.portable_marker() {
        let exe_dir = marker.parent().unwrap_or(Path::new(""));
        return Ok(fresh(Source::Portable, &exe_dir.join(DATA_SUBDIR), None));
    }
    match read_bootstrap(&dirs.bootstrap_path())? {
        Some(b) => {
            let (source, dir) = locate(dirs, &b.current);
            check_existing(source, &dir, &b.previous)?;
            Ok(Resolved {
                source,
                layout: DataLayout::in_dir(&dir),
                creation: Creation::MustExist,
                persist: None,
                previous: b.previous,
                legacy_dir: None,
            })
        }
        None => {
            let legacy_dir = dirs
                .exe_dir
                .as_ref()
                .filter(|d| d.join(DB_FILE).is_file())
                .cloned();
            let mut r = fresh(
                Source::System,
                &dirs.system_data_dir(),
                Some(Bootstrap::new(Choice::system(), None)),
            );
            r.legacy_dir = legacy_dir;
            Ok(r)
        }
    }
}

/// Open — or, where [`Resolved::creation`] allows it, create — the database,
/// and only then record the choice.
///
/// Recording after opening matters on first launch: a bootstrap written
/// before a failed create would turn the next launch into "your data is
/// missing" for data that never existed.
pub fn prepare(dirs: &SystemDirs, r: &Resolved) -> Result<Prepared, StartupError> {
    let dir = &r.layout.dir;
    let db = &r.layout.db;
    let exists = db.exists();
    let unavailable = |why| StartupError::DataUnavailable {
        source: r.source,
        dir: dir.clone(),
        why,
        previous: r.previous.clone(),
    };
    match (r.creation, exists) {
        (Creation::MustBeNew, true) => {
            return Err(StartupError::DatabaseExists { dir: dir.clone() })
        }
        (Creation::MustExist, false) => {
            check_existing(r.source, dir, &r.previous)?;
        }
        _ => {}
    }
    if !exists {
        fs::create_dir_all(dir)
            .map_err(|e| unavailable(Unavailable::NotWritable(e.to_string())))?;
    }
    probe_writable(dir).map_err(|e| unavailable(Unavailable::NotWritable(e.to_string())))?;
    if exists {
        open_existing(db).map_err(unavailable)?;
    } else {
        pc_db::Db::open(db).map_err(|e| StartupError::CreateFailed {
            dir: dir.clone(),
            reason: format!("{e:#}"),
        })?;
    }
    // Only bootstrap-backed sources carry `persist`; a portable or
    // `--data-dir` launch must leave nothing in the user profile.
    if let (Some(b), Source::System | Source::Custom) = (&r.persist, r.source) {
        write_bootstrap(&dirs.bootstrap_path(), b)?;
    }
    Ok(Prepared {
        source: r.source,
        layout: r.layout.clone(),
        created: !exists,
    })
}

/// The server is up on the chosen directory: a choice that was waiting for
/// this stops being provisional, and "go back" is no longer offered.
pub fn confirm_started(dirs: &SystemDirs, p: &Prepared) -> Result<(), StartupError> {
    if !matches!(p.source, Source::System | Source::Custom) {
        return Ok(());
    }
    let path = dirs.bootstrap_path();
    if let Some(mut b) = read_bootstrap(&path)? {
        if b.previous.is_some() {
            b.previous = None;
            write_bootstrap(&path, &b)?;
        }
    }
    Ok(())
}

/// Use a folder the user picked as the data directory from now on.
///
/// This records a choice; it does not move data. Copying an archive to a new
/// place is a separate operation (DESKTOP.md, "Смена каталога данных") that
/// ends by writing the same kind of bootstrap.
pub fn choose_data_dir(
    dirs: &SystemDirs,
    dir: &Path,
    what: NewDir,
) -> Result<Resolved, StartupError> {
    if let Some(marker) = dirs.portable_marker() {
        return Err(StartupError::PortableActive { marker });
    }
    require_absolute(dir)?;
    let path = dirs.bootstrap_path();
    // An unreadable bootstrap is exactly what the user may be choosing their
    // way out of, so it does not block an explicit choice. One from a newer
    // build does: rewriting it would downgrade that build's settings.
    let current = match read_bootstrap(&path) {
        Ok(b) => b.map(|b| b.current),
        Err(StartupError::BootstrapUnreadable { .. }) => None,
        Err(e) => return Err(e),
    };
    let choice = if dir == dirs.system_data_dir() {
        Choice::system()
    } else {
        Choice::custom(dir.to_path_buf())
    };
    let (source, _) = locate(dirs, &choice);
    let creation = match what {
        NewDir::UseExisting => {
            check_existing(source, dir, &None)?;
            Creation::MustExist
        }
        NewDir::CreateNew => {
            if dir.join(DB_FILE).exists() {
                return Err(StartupError::DatabaseExists {
                    dir: dir.to_path_buf(),
                });
            }
            Creation::MustBeNew
        }
    };
    let previous = current.filter(|c| *c != choice);
    Ok(Resolved {
        source,
        layout: DataLayout::in_dir(dir),
        creation,
        persist: Some(Bootstrap::new(choice, previous)),
        previous: None,
        legacy_dir: None,
    })
}

/// Go back to the choice before the last change — the way out when a new
/// data directory does not come up.
pub fn revert_to_previous(dirs: &SystemDirs) -> Result<Resolved, StartupError> {
    if let Some(marker) = dirs.portable_marker() {
        return Err(StartupError::PortableActive { marker });
    }
    let previous = read_bootstrap(&dirs.bootstrap_path())?
        .and_then(|b| b.previous)
        .ok_or(StartupError::NoPrevious)?;
    let (source, dir) = locate(dirs, &previous);
    check_existing(source, &dir, &None)?;
    Ok(Resolved {
        source,
        layout: DataLayout::in_dir(&dir),
        creation: Creation::MustExist,
        persist: Some(Bootstrap::new(previous, None)),
        previous: None,
        legacy_dir: None,
    })
}

fn fresh(source: Source, dir: &Path, persist: Option<Bootstrap>) -> Resolved {
    Resolved {
        source,
        layout: DataLayout::in_dir(dir),
        creation: Creation::MayCreate,
        persist,
        previous: None,
        legacy_dir: None,
    }
}

fn locate(dirs: &SystemDirs, c: &Choice) -> (Source, PathBuf) {
    match (c.mode, &c.data_dir) {
        (StoredMode::Custom, Some(d)) => (Source::Custom, d.clone()),
        _ => (Source::System, dirs.system_data_dir()),
    }
}

fn require_absolute(dir: &Path) -> Result<(), StartupError> {
    if dir.is_absolute() {
        Ok(())
    } else {
        Err(StartupError::RelativePath {
            path: dir.to_path_buf(),
        })
    }
}

/// The directory is there and holds a database. Reads only: in particular
/// it never creates the directory, which for an unplugged drive would put an
/// empty archive on the boot disk under the drive's mount point.
fn check_existing(
    source: Source,
    dir: &Path,
    previous: &Option<Choice>,
) -> Result<(), StartupError> {
    let why = match fs::metadata(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(Unavailable::Missing),
        Err(e) => Some(Unavailable::NotWritable(e.to_string())),
        Ok(m) if !m.is_dir() => Some(Unavailable::NotADirectory),
        Ok(_) if !dir.join(DB_FILE).is_file() => Some(Unavailable::NoDatabase),
        Ok(_) => None,
    };
    match why {
        None => Ok(()),
        Some(why) => Err(StartupError::DataUnavailable {
            source,
            dir: dir.to_path_buf(),
            why,
            previous: previous.clone(),
        }),
    }
}

/// SQLite in WAL mode writes `-wal`, `-shm` and our `.writer-lock` beside the
/// database, so the directory must be writable, not only the file. Asking
/// the permission bits is not enough (ACLs, read-only mounts), so try.
fn probe_writable(dir: &Path) -> std::io::Result<()> {
    let probe = dir.join(".photo-cleanup-write-probe");
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)
}

/// Open an existing database without the power to create one, so that a
/// file vanishing between the check and the open is an error rather than a
/// new empty archive; then let `pc-db` migrate it as the server would.
fn open_existing(db: &Path) -> Result<(), Unavailable> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Unavailable::NotADatabase(e.to_string()))?;
    // A write-protected file is opened read-only without complaint.
    if conn
        .is_readonly(MAIN_DB)
        .map_err(|e| Unavailable::NotADatabase(e.to_string()))?
    {
        return Err(Unavailable::NotWritable(db.display().to_string()));
    }
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| Unavailable::NotADatabase(e.to_string()))?;
    drop(conn);
    pc_db::Db::open(db).map_err(|e| Unavailable::NotADatabase(format!("{e:#}")))?;
    Ok(())
}
