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
use std::io;
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
/// database, so the directory must let this process create, write and
/// delete regular files. Bare permission bits are not enough (ACLs,
/// read-only mounts); the kernel is asked or, where it can be done without
/// leaving a name behind, a file is actually written.
///
/// The directory may be the user's archive, so the probe never gives an
/// entry a name that would later have to be removed by path. Removing a
/// path "only if it is still ours" cannot be done atomically on Linux or
/// macOS: between any identity check and `unlink`/`rmdir` another process
/// can move its own file, symlink or (empty, xattr-carrying) directory onto
/// the name, and the removal would take that instead. So no directory entry
/// is ever created, and nothing is ever removed:
///
/// - Linux: an unnamed `O_TMPFILE | O_EXCL` file is created in the
///   directory and written to. It never appears in the directory, cannot be
///   linked into it later, and the kernel frees it when it is closed. File
///   systems without `O_TMPFILE` fall back to asking the kernel, as below.
/// - macOS: the kernel authorization check, `faccessat` with the extended
///   rights "add file", "search" and "remove file" (`_WRITE_OK`,
///   `_EXECUTE_OK`, `_RMFILE_OK`). It honours ACLs and read-only mounts,
///   and it asks for file rights, not the "add subdirectory" right that
///   `mkdir` would need. Nothing is created. It is the kernel's answer, not
///   a trial write: a network server or a full disk can still refuse later,
///   which then surfaces as a database open error.
/// - Other Unix: `faccessat(W_OK | X_OK)`, nothing is created.
/// - Windows: a uniquely named file created with `CREATE_NEW`, written, held
///   with no sharing and `FILE_FLAG_DELETE_ON_CLOSE`. While it is open nobody
///   can open, rename, delete or replace it, and the system deletes that
///   file — by handle, never by path — when it is closed. A taken name
///   (file, directory, link) is skipped, never opened.
///
/// Any failure is an error; success is never reported for a probe that
/// could not be done.
fn probe_writable(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return probe_writable_with(dir, || {});
    #[cfg(windows)]
    return probe_writable_with(dir, probe_name, |_| {});
}

/// `during` runs while the probe is live: after the unnamed file was
/// written and before it is closed. Tests act there; production passes a
/// no-op.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn probe_writable_with(dir: &Path, during: impl FnOnce()) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let opened = fs::OpenOptions::new()
        .write(true)
        .mode(0o600)
        // `O_EXCL` also forbids ever linking it into the directory.
        .custom_flags(libc::O_TMPFILE | libc::O_EXCL)
        .open(dir);
    match opened {
        Ok(mut file) => {
            file.write_all(b"photo-cleanup write probe")?;
            during();
            Ok(()) // closing frees it; it never had a name
        }
        // No `O_TMPFILE` on this file system (`EOPNOTSUPP`) or kernel
        // (`EISDIR`).
        Err(e) if matches!(e.raw_os_error(), Some(libc::EOPNOTSUPP | libc::EISDIR)) => {
            ask_kernel(dir)?;
            during();
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn probe_writable_with(dir: &Path, during: impl FnOnce()) -> io::Result<()> {
    ask_kernel(dir)?;
    during();
    Ok(())
}

/// Rights to add, search for and remove files in `dir`, as the kernel would
/// decide them for this process. Creates nothing.
#[cfg(unix)]
fn ask_kernel(dir: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    // <sys/unistd.h>: the kauth rights shifted left by 8. For a directory,
    // `_WRITE_OK` is "add file", `_RMFILE_OK` is "delete child".
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    const RIGHTS: (libc::c_int, libc::c_int) = {
        const WRITE: libc::c_int = 1 << 10;
        const EXECUTE: libc::c_int = 1 << 11;
        const RMFILE: libc::c_int = 1 << 14;
        (WRITE | EXECUTE | RMFILE, libc::AT_EACCESS)
    };
    // The real ids, as `access(2)`: glibc emulates `AT_EACCESS` from mode
    // bits on kernels without `faccessat2`, which would skip ACLs.
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    const RIGHTS: (libc::c_int, libc::c_int) = (libc::W_OK | libc::X_OK, 0);
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
    if unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), RIGHTS.0, RIGHTS.1) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
const PROBE_PREFIX: &str = ".photo-cleanup-write-probe";
#[cfg(windows)]
const PROBE_ATTEMPTS: u32 = 16;

#[cfg(windows)]
fn probe_name(_attempt: u32) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{PROBE_PREFIX}-{}-{nanos:x}-{n}", std::process::id())
}

/// `during` runs while the probe file is open, with its path. Tests act
/// there; production passes a no-op.
#[cfg(windows)]
fn probe_writable_with(
    dir: &Path,
    mut name: impl FnMut(u32) -> String,
    during: impl FnOnce(&Path),
) -> io::Result<()> {
    use std::io::Write;
    use std::os::windows::fs::OpenOptionsExt;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const DELETE: u32 = 0x0001_0000;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    for attempt in 0..PROBE_ATTEMPTS {
        let path = dir.join(name(attempt));
        let opened = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE | DELETE)
            .share_mode(0)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(&path);
        match opened {
            Ok(mut file) => {
                file.write_all(b"photo-cleanup write probe")?;
                during(&path);
                // Deleted by handle on close; without sharing nobody could
                // have opened, renamed or replaced it in the meantime.
                drop(file);
                return Ok(());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name for the write probe",
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    const LEGACY: &str = ".photo-cleanup-write-probe";

    /// Every entry of `dir` with its bytes, or its link target for a symlink.
    fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let name = e.file_name().to_string_lossy().into_owned();
                let ty = e.file_type().unwrap();
                let bytes = if ty.is_symlink() {
                    let t = fs::read_link(e.path()).unwrap();
                    format!("-> {}", t.display()).into_bytes()
                } else if ty.is_dir() {
                    b"<dir>".to_vec()
                } else {
                    fs::read(e.path()).unwrap()
                };
                (name, bytes)
            })
            .collect()
    }

    /// Entries a probe must leave byte-identical, including the fixed name
    /// the old probe truncated and removed.
    fn foreign(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::write(dir.join(LEGACY), b"legacy payload").unwrap();
        fs::write(dir.join(format!("{LEGACY}-taken")), b"user bytes \x00\xff").unwrap();
        fs::write(dir.join("photo.jpg"), b"\xff\xd8 jpeg").unwrap();
        snapshot(dir)
    }

    #[test]
    fn a_probe_leaves_nothing_and_touches_no_existing_file() {
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        probe_writable(tmp.path()).unwrap();
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn parallel_probes_leave_only_foreign_entries() {
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    for _ in 0..20 {
                        probe_writable(tmp.path()).unwrap();
                    }
                });
            }
        });
        assert_eq!(snapshot(tmp.path()), before);
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let tmp = TempDir::new().unwrap();
        assert!(probe_writable(&tmp.path().join("missing")).is_err());
        assert!(snapshot(tmp.path()).is_empty());
    }

    /// Set an extended attribute; `false` where the file system has none.
    #[cfg(unix)]
    fn set_xattr(path: &Path, value: &[u8]) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let p = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = c"user.photo-cleanup-test";
        let v = value.as_ptr().cast();
        // SAFETY: valid NUL-terminated strings and a live buffer.
        #[cfg(target_os = "macos")]
        let r = unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), v, value.len(), 0, 0) };
        #[cfg(not(target_os = "macos"))]
        let r = unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), v, value.len(), 0) };
        r == 0
    }

    #[cfg(unix)]
    fn get_xattr(path: &Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;
        let p = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let name = c"user.photo-cleanup-test";
        let mut buf = vec![0u8; 256];
        let b = buf.as_mut_ptr().cast();
        // SAFETY: valid NUL-terminated strings and a live buffer.
        #[cfg(target_os = "macos")]
        let n = unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), b, buf.len(), 0, 0) };
        #[cfg(not(target_os = "macos"))]
        let n = unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), b, buf.len()) };
        assert!(n >= 0, "{}", io::Error::last_os_error());
        buf.truncate(n as usize);
        buf
    }

    /// The review races (el-1e5d, el-2d03): while the probe is live, other
    /// processes move a file, symlinks and an empty directory carrying an
    /// xattr onto probe-like names. There is no probe entry to confuse them
    /// with — nothing named appears — so every one of them survives as it
    /// was, and nothing is removed.
    #[cfg(unix)]
    #[test]
    fn entries_moved_in_while_the_probe_is_live_all_survive() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("target.jpg");
        fs::write(&target, b"target payload").unwrap();
        let before = foreign(tmp.path());
        let empty = outside.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let has_xattr = set_xattr(&empty, b"their metadata");
        let file = outside.path().join("file");
        fs::write(&file, b"someone else's \x00 bytes").unwrap();
        let outside_before = snapshot(outside.path());

        let mut seen = None;
        probe_writable_with(tmp.path(), || {
            seen = Some(snapshot(tmp.path()));
            fs::rename(&empty, tmp.path().join(format!("{LEGACY}-raced-dir"))).unwrap();
            fs::rename(&file, tmp.path().join(format!("{LEGACY}-raced-file"))).unwrap();
            symlink(&target, tmp.path().join(format!("{LEGACY}-raced-link"))).unwrap();
            symlink(
                outside.path().join("missing"),
                tmp.path().join(format!("{LEGACY}-dangling")),
            )
            .unwrap();
        })
        .unwrap();

        assert_eq!(
            seen.unwrap(),
            before,
            "the probe must never be a named entry"
        );
        let after = snapshot(tmp.path());
        for (name, bytes) in &before {
            assert_eq!(after.get(name), Some(bytes), "{name}");
        }
        let dir = tmp.path().join(format!("{LEGACY}-raced-dir"));
        assert!(dir.is_dir());
        if has_xattr {
            assert_eq!(get_xattr(&dir), b"their metadata");
        }
        assert_eq!(
            fs::read(tmp.path().join(format!("{LEGACY}-raced-file"))).unwrap(),
            b"someone else's \x00 bytes"
        );
        assert_eq!(
            fs::read_link(tmp.path().join(format!("{LEGACY}-raced-link"))).unwrap(),
            target
        );
        assert_eq!(
            fs::read_link(tmp.path().join(format!("{LEGACY}-dangling"))).unwrap(),
            outside.path().join("missing")
        );
        assert_eq!(fs::read(&target).unwrap(), b"target payload");
        assert!(!outside.path().join("missing").exists());
        assert_eq!(after.len(), before.len() + 4);
        let mut outside_after = snapshot(outside.path());
        outside_after.insert("empty".into(), b"<dir>".to_vec());
        outside_after.insert("file".into(), b"someone else's \x00 bytes".to_vec());
        assert_eq!(outside_after, outside_before);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_at_probe_like_names_are_not_followed() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("target.jpg");
        fs::write(&target, b"target payload").unwrap();
        symlink(&target, tmp.path().join(LEGACY)).unwrap();
        symlink(
            outside.path().join("missing"),
            tmp.path().join(format!("{LEGACY}-x")),
        )
        .unwrap();
        let before = snapshot(tmp.path());
        let outside_before = snapshot(outside.path());

        probe_writable(tmp.path()).unwrap();

        assert_eq!(snapshot(tmp.path()), before);
        assert_eq!(snapshot(outside.path()), outside_before);
        assert!(!outside.path().join("missing").exists());
    }

    /// `umask` is process-wide, so this runs the probe in a child copy of
    /// the test binary rather than beside the other tests.
    #[cfg(unix)]
    #[test]
    fn a_probe_under_a_restrictive_umask_leaves_nothing() {
        const CHILD: &str = "PC_DESKTOP_PROBE_UMASK_DIR";
        if let Some(dir) = std::env::var_os(CHILD) {
            // SAFETY: umask has no preconditions; this child process runs
            // only this test, on one thread.
            let old = unsafe { libc::umask(0o444) };
            let result = probe_writable(Path::new(&dir));
            unsafe { libc::umask(old) };
            result.unwrap();
            return;
        }
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "resolve::tests::a_probe_under_a_restrictive_umask_leaves_nothing",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD, tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(snapshot(tmp.path()), before);
    }

    /// Mode bits are ignored for root; such a run cannot test refusal.
    #[cfg(unix)]
    fn bits_bind(dir: &Path) -> bool {
        fs::write(dir.join(".root"), b"").is_err()
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_fails_and_keeps_its_entries() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let binds = bits_bind(tmp.path());
        let result = probe_writable(tmp.path());
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        if !binds {
            return;
        }
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(snapshot(tmp.path()), before);
    }

    /// The tests above exercise the unnamed file, not the fallback, where the
    /// temporary directory supports `O_TMPFILE` (tmpfs, ext4, overlayfs).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn the_unnamed_file_is_what_is_tested_here() {
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = TempDir::new().unwrap();
        fs::OpenOptions::new()
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_TMPFILE | libc::O_EXCL)
            .open(tmp.path())
            .expect("O_TMPFILE unsupported here; only the fallback was tested");
        assert!(snapshot(tmp.path()).is_empty());
    }

    /// The fallback for file systems without `O_TMPFILE` answers the same.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn the_kernel_check_fallback_agrees() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        ask_kernel(tmp.path()).unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let binds = bits_bind(tmp.path());
        let result = ask_kernel(tmp.path());
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        if binds {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
        assert_eq!(snapshot(tmp.path()), before);
    }

    /// A directory with one ACL entry for the current user, removed again
    /// when dropped so the temporary directory can be deleted.
    #[cfg(target_os = "macos")]
    struct Acl(TempDir);

    #[cfg(target_os = "macos")]
    impl Acl {
        fn deny(right: &str) -> Self {
            let tmp = TempDir::new().unwrap();
            let user = std::env::var("USER").unwrap();
            let ok = std::process::Command::new("/bin/chmod")
                .arg("+a")
                .arg(format!("user:{user} deny {right}"))
                .arg(tmp.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "chmod +a deny {right}");
            Self(tmp)
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for Acl {
        fn drop(&mut self) {
            let _ = std::process::Command::new("/bin/chmod")
                .arg("-N")
                .arg(self.0.path())
                .status();
        }
    }

    /// File create/write/delete allowed, subdirectories denied: that is all
    /// SQLite needs, so the folder is writable (el-2d03).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_folder_that_denies_only_subdirectories_is_writable() {
        let acl = Acl::deny("add_subdirectory");
        let dir = acl.0.path();
        assert!(
            fs::create_dir(dir.join("sub")).is_err(),
            "ACL not in effect"
        );
        let before = snapshot(dir);
        probe_writable(dir).unwrap();
        assert_eq!(snapshot(dir), before);
        // What the probe promised really works.
        fs::write(dir.join("f"), b"x").unwrap();
        fs::remove_file(dir.join("f")).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_folder_that_denies_adding_files_is_not_writable() {
        let acl = Acl::deny("add_file");
        let dir = acl.0.path();
        assert!(fs::write(dir.join("f"), b"x").is_err(), "ACL not in effect");
        let e = probe_writable(dir).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert!(snapshot(dir).is_empty());
    }

    /// SQLite deletes its `-wal` and journal files; a folder where that is
    /// denied would fail later, so it fails here.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_folder_that_denies_deleting_files_is_not_writable() {
        let acl = Acl::deny("delete_child");
        let dir = acl.0.path();
        let before = foreign(dir);
        let e = probe_writable(dir).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(snapshot(dir), before);
    }

    #[cfg(windows)]
    #[test]
    fn probe_names_are_unique() {
        let a = probe_name(0);
        let b = probe_name(0);
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("{PROBE_PREFIX}-")));
    }

    #[cfg(windows)]
    #[test]
    fn a_taken_name_is_skipped_not_truncated_or_removed() {
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        fs::create_dir(tmp.path().join(format!("{PROBE_PREFIX}-dir"))).unwrap();
        let before_dir = snapshot(tmp.path());
        let mut tried = Vec::new();
        probe_writable_with(
            tmp.path(),
            |i| {
                tried.push(i);
                match i {
                    0 => format!("{PROBE_PREFIX}-taken"),
                    1 => format!("{PROBE_PREFIX}-dir"),
                    _ => format!("{PROBE_PREFIX}-free"),
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(tried, [0, 1, 2]);
        assert_eq!(snapshot(tmp.path()), before_dir);
        assert!(before_dir.len() == before.len() + 1);
    }

    #[cfg(windows)]
    #[test]
    fn every_name_taken_is_an_error_that_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        let e = probe_writable_with(tmp.path(), |_| format!("{PROBE_PREFIX}-taken"), |_| {})
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(snapshot(tmp.path()), before);
    }

    /// While the probe is open it cannot be opened, renamed, deleted or
    /// replaced — that is what makes the delete-on-close safe — and once it
    /// is closed it is gone.
    #[cfg(windows)]
    #[test]
    fn the_live_probe_is_exclusive_and_deleted_on_close() {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let tmp = TempDir::new().unwrap();
        let before = foreign(tmp.path());
        let theirs = tmp.path().join("theirs");
        fs::write(&theirs, b"someone else's bytes").unwrap();
        let mut probe = None;
        probe_writable_with(
            tmp.path(),
            |_| format!("{PROBE_PREFIX}-live"),
            |path| {
                probe = Some(path.to_path_buf());
                let sharing = |r: io::Result<()>| {
                    assert_eq!(r.unwrap_err().raw_os_error(), Some(ERROR_SHARING_VIOLATION));
                };
                sharing(fs::File::open(path).map(drop));
                sharing(fs::rename(path, tmp.path().join("moved")));
                sharing(fs::remove_file(path));
                // Replacing it needs the same delete access.
                assert!(fs::rename(&theirs, path).is_err());
            },
        )
        .unwrap();
        let probe = probe.unwrap();
        assert!(fs::symlink_metadata(&probe).is_err(), "probe left behind");
        assert_eq!(fs::read(&theirs).unwrap(), b"someone else's bytes");
        let mut after = snapshot(tmp.path());
        after.remove("theirs");
        assert_eq!(after, before);
    }
}
