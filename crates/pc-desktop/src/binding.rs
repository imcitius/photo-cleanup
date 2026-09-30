//! Opening a bound data folder: the objects at the recorded path must be the
//! ones the move proved, before SQLite sees them, and stay so while the
//! server runs on them.
//!
//! A [`Binding`] is written by [`crate::relocate`] after the copy was
//! proven. At start-up [`DataGuard::open`] — before any SQLite open, `-wal`,
//! `-shm`, migration or writer lock:
//!
//! 1. admits the data folder's path as a protected namespace
//!    ([`crate::namespace`]): nobody but this user and root can redirect
//!    it, so the path the server is given later leads to the same objects;
//! 2. opens the folder, the database file (read-only, never created, no
//!    link followed, not blocking on a FIFO) and the thumbnail folder
//!    relative to it, and compares volume, inodes and the generation on the
//!    database file with the binding.
//!
//! Any difference is a refusal naming the path and the difference; nothing
//! is created, written or migrated there. The guard keeps these objects
//! open, and [`DataGuard::verify`] repeats the walk and compares the path's
//! objects with the held ones — after the database was opened for
//! migration, and after the server started on the path — so a substitution
//! by the user's own programs in between is refused rather than served.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::bootstrap::Binding;
use crate::namespace::{self, FileUse, Protected};
use crate::{DB_FILE, THUMBS_DIR};

/// The extended attribute holding a copy's generation.
#[cfg(target_os = "macos")]
const GENERATION_ATTR: &std::ffi::CStr = c"io.github.imcitius.photo-cleanup.generation";
#[cfg(target_os = "linux")]
const GENERATION_ATTR: &std::ffi::CStr = c"user.photo-cleanup.generation";

/// A bound data folder, proven and held open.
#[derive(Debug)]
pub struct DataGuard {
    chain: Protected,
    dir: PathBuf,
    db: fs::File,
    thumbs: fs::File,
    binding: Binding,
}

impl PartialEq for DataGuard {
    fn eq(&self, other: &Self) -> bool {
        self.dir == other.dir && self.binding == other.binding
    }
}

impl Eq for DataGuard {}

impl DataGuard {
    /// Prove that `dir` holds the objects `binding` names, before anything
    /// opens them for writing: the folder on a protected path
    /// ([`crate::namespace`]), the volume, the folder, the database file
    /// (with this copy's generation) and the thumbnail folder, each also
    /// protected on its own ([`DataGuard::check_database`],
    /// [`DataGuard::check_thumbnails`]).
    pub fn open(dir: &Path, binding: &Binding) -> Result<Self, String> {
        let chain = namespace::admit_existing(dir).map_err(|r| r.to_string())?;
        if chain.path() != dir {
            return Err(pc_core::tf!(
                "путь {0} ведёт через ссылку в {1}; привязанная папка записана без ссылок",
                "the path {0} goes through a link to {1}; a bound folder is recorded without links",
                dir.display(),
                chain.path().display()
            ));
        }
        let folder = chain.dir().expect("admit_existing");
        let volume = namespace::volume_id(folder).map_err(|e| {
            pc_core::tf!(
                "не узнать идентификатор тома: {0}",
                "the volume's identity cannot be read: {0}",
                e
            )
        })?;
        if volume != binding.volume {
            return Err(refused(dir, pc_core::tr!("другой том", "another volume")));
        }
        if inode(folder).map_err(|e| e.to_string())? != binding.dir {
            return Err(refused(dir, pc_core::tr!("другая папка", "another folder")));
        }
        let db_path = dir.join(DB_FILE);
        let db = platform::open_file(folder, DB_FILE).map_err(|e| {
            refused(
                &db_path,
                &pc_core::tf!(
                    "не открывается как файл: {0}",
                    "cannot be opened as a file: {0}",
                    e
                ),
            )
        })?;
        if inode(&db).map_err(|e| e.to_string())? != binding.db {
            return Err(refused(
                &db_path,
                pc_core::tr!("другой файл", "another file"),
            ));
        }
        if read_generation(&db).ok().flatten().as_deref() != Some(binding.generation.as_str()) {
            return Err(refused(
                &db_path,
                pc_core::tr!(
                    "у файла нет метки этой копии",
                    "the file does not carry this copy's generation"
                ),
            ));
        }
        let thumbs_path = dir.join(THUMBS_DIR);
        let thumbs = platform::open_dir(folder, THUMBS_DIR).map_err(|e| {
            refused(
                &thumbs_path,
                &pc_core::tf!(
                    "не открывается как папка: {0}",
                    "cannot be opened as a folder: {0}",
                    e
                ),
            )
        })?;
        if inode(&thumbs).map_err(|e| e.to_string())? != binding.thumbs {
            return Err(refused(
                &thumbs_path,
                pc_core::tr!("другая папка", "another folder"),
            ));
        }
        let guard = Self {
            chain,
            dir: dir.to_path_buf(),
            db,
            thumbs,
            binding: binding.clone(),
        };
        guard.database()?;
        guard.thumbnails()?;
        Ok(guard)
    }

    /// Both [`DataGuard::check_database`] and
    /// [`DataGuard::check_thumbnails`].
    pub fn verify(&self) -> Result<(), String> {
        self.database()?;
        self.thumbnails()
    }

    /// Right before the database, one of SQLite's files beside it or the
    /// writer lock is opened for writing:
    ///
    /// - every folder on the path is still the proven one and still
    ///   protected ([`crate::namespace::Protected::recheck`]);
    /// - the path, resolved by the system exactly as SQLite will resolve
    ///   it, leads to the proven database file, and that file is still a
    ///   plain file with one name, this user's, not writable or
    ///   re-permissionable by anybody else, carrying this copy's generation;
    /// - `-wal`, `-shm`, `-journal` and `.writer-lock` beside it are absent
    ///   or this user's plain single-name files (the SQLite ones also
    ///   protected like the database). SQLite and the lock open these by
    ///   name and write into whatever is there; a link would pass the
    ///   write on to somebody else's file.
    fn database(&self) -> Result<(), String> {
        self.chain.recheck().map_err(|r| r.to_string())?;
        let folder = self.chain.dir().expect("admit_existing");
        let db_path = self.dir.join(DB_FILE);
        self.same(&db_path, &self.db, false)?;
        let at = namespace::check_file_at(folder, DB_FILE, FileUse::Contents)
            .map_err(|why| refused(&db_path, &why))?;
        if at != Some(file_identity(&self.db).map_err(|e| e.to_string())?) {
            return Err(self.replaced(&db_path, false));
        }
        if read_generation(&self.db).ok().flatten().as_deref()
            != Some(self.binding.generation.as_str())
        {
            return Err(refused(
                &db_path,
                pc_core::tr!(
                    "у файла больше нет метки этой копии",
                    "the file no longer carries this copy's generation"
                ),
            ));
        }
        for (suffix, what) in COMPANIONS {
            let name = format!("{DB_FILE}{suffix}");
            namespace::check_file_at(folder, &name, what)
                .map_err(|why| refused(&self.dir.join(&name), &why))?;
        }
        Ok(())
    }

    /// Right before anything in the thumbnail cache is written or removed:
    /// the path leads to the proven folder, and that folder is still this
    /// user's and nobody else can change it
    /// ([`crate::namespace::check_own_folder`]). Asked for every thumbnail
    /// stored, so it looks only at the cache itself; the folders above it
    /// are walked again before every database open and writer lock, which
    /// every job and change goes through first.
    fn thumbnails(&self) -> Result<(), String> {
        let path = self.dir.join(THUMBS_DIR);
        self.same(&path, &self.thumbs, true)?;
        namespace::check_own_folder(&self.thumbs).map_err(|why| refused(&path, &why))
    }

    /// `path`, resolved the way every later user of it resolves it, is the
    /// held object.
    fn same(&self, path: &Path, held: &fs::File, directory: bool) -> Result<(), String> {
        let now = fs::symlink_metadata(path).map_err(|e| refused(path, &e.to_string()))?;
        let kind_ok = if directory {
            now.is_dir()
        } else {
            now.is_file()
        };
        if kind_ok && identity(&now) == file_identity(held).map_err(|e| e.to_string())? {
            Ok(())
        } else {
            Err(self.replaced(path, directory))
        }
    }

    /// A proven object is no longer at its name. Where it is now, if the
    /// system can tell from the descriptor held on it; nothing is searched.
    fn replaced(&self, path: &Path, directory: bool) -> String {
        let held = if directory { &self.thumbs } else { &self.db };
        let located = match platform::current_path(held) {
            Some(now) if now != path => pc_core::tf!(
                "проверенный объект сейчас находится по пути {0}",
                "the proven one is now at {0}",
                now.display()
            ),
            _ => pc_core::tr!(
                "где сейчас проверенный объект, система не сообщает",
                "the system does not tell where the proven one is now"
            )
            .to_string(),
        };
        refused(
            path,
            &pc_core::tf!(
                "подменён после проверки; {0}",
                "was replaced after the check; {0}",
                located
            ),
        )
    }
}

impl pc_core::storage::StorageBinding for DataGuard {
    fn check_database(&self) -> Result<(), String> {
        self.database()
    }

    fn check_thumbnails(&self) -> Result<(), String> {
        self.thumbnails()
    }
}

/// What SQLite and the writer lock open beside the database by name.
const COMPANIONS: [(&str, FileUse); 4] = [
    ("-wal", FileUse::Contents),
    ("-shm", FileUse::Contents),
    ("-journal", FileUse::Contents),
    (".writer-lock", FileUse::Note),
];

/// Every refusal comes before a write: the checks run right before the
/// database, its companions, the lock or the cache are touched, and a
/// refusal stops that. So "nothing was written there" is true of whatever
/// now stands at the name.
fn refused(path: &Path, what: &str) -> String {
    pc_core::tf!(
        "{0}: не та копия, которую программа проверила и выбрала ({1}); туда ничего не \
         записано",
        "{0}: not the copy this program proved and chose ({1}); nothing was written there",
        path.display(),
        what
    )
}

pub(crate) fn new_generation() -> io::Result<String> {
    platform::random().map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// Write `generation` onto the file open as `db`.
pub(crate) fn set_generation(db: &fs::File, generation: &str) -> io::Result<()> {
    platform::set_attr(db, generation.as_bytes())
}

/// The generation on the file open as `db`, if any.
pub(crate) fn read_generation(db: &fs::File) -> io::Result<Option<String>> {
    Ok(platform::get_attr(db)?.map(|v| String::from_utf8_lossy(&v).into_owned()))
}

pub(crate) fn inode(file: &fs::File) -> io::Result<u64> {
    file_identity(file).map(|(_, ino)| ino)
}

fn file_identity(file: &fs::File) -> io::Result<(u64, u64)> {
    file.metadata().map(|m| identity(&m))
}

#[cfg(unix)]
fn identity(m: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino())
}

#[cfg(not(unix))]
fn identity(_: &fs::Metadata) -> (u64, u64) {
    (u64::MAX, u64::MAX)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod platform {
    use super::GENERATION_ATTR;
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::{fs, io};

    fn open_at(dir: &fs::File, name: &str, flags: libc::c_int) -> io::Result<fs::File> {
        let name = CString::new(name)?;
        // SAFETY: an open descriptor and a NUL-terminated name; the new
        // descriptor is owned by the `File`.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fresh descriptor.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    /// Read-only, not created, not a link; `O_NONBLOCK` so a FIFO put there
    /// does not hang the start. Must be a regular file.
    pub(super) fn open_file(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        let file = open_at(dir, name, libc::O_RDONLY | libc::O_NONBLOCK)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("not a regular file"));
        }
        Ok(file)
    }

    pub(super) fn open_dir(dir: &fs::File, name: &str) -> io::Result<fs::File> {
        open_at(dir, name, libc::O_RDONLY | libc::O_DIRECTORY)
    }

    /// Where the object open as `file` is now, as the system names it.
    #[cfg(target_os = "macos")]
    pub(super) fn current_path(file: &fs::File) -> Option<std::path::PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes into `buf`.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0)?;
        Some(std::ffi::OsStr::from_bytes(&buf[..end]).into())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn current_path(file: &fs::File) -> Option<std::path::PathBuf> {
        let path = fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
        // A removed file is shown with this suffix; it has no place then.
        (!path.to_string_lossy().ends_with(" (deleted)")).then_some(path)
    }

    pub(super) fn random() -> io::Result<[u8; 16]> {
        let mut buf = [0u8; 16];
        // SAFETY: `buf` is writable for its length (≤ 256).
        if unsafe { libc::getentropy(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(buf)
    }

    pub(super) fn set_attr(file: &fs::File, value: &[u8]) -> io::Result<()> {
        // SAFETY: valid descriptor, NUL-terminated name, `value` readable.
        #[cfg(target_os = "macos")]
        let rc = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                GENERATION_ATTR.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        // SAFETY: as above.
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::fsetxattr(
                file.as_raw_fd(),
                GENERATION_ATTR.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn get_attr(file: &fs::File) -> io::Result<Option<Vec<u8>>> {
        let mut buf = vec![0u8; 256];
        // SAFETY: valid descriptor, NUL-terminated name, `buf` writable.
        #[cfg(target_os = "macos")]
        let n = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                GENERATION_ATTR.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                0,
                0,
            )
        };
        // SAFETY: as above.
        #[cfg(target_os = "linux")]
        let n = unsafe {
            libc::fgetxattr(
                file.as_raw_fd(),
                GENERATION_ATTR.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            #[cfg(target_os = "macos")]
            let absent = libc::ENOATTR;
            #[cfg(target_os = "linux")]
            let absent = libc::ENODATA;
            return if e.raw_os_error() == Some(absent) {
                Ok(None)
            } else {
                Err(e)
            };
        }
        buf.truncate(n as usize);
        Ok(Some(buf))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use std::{fs, io};

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "bound data folders cannot be verified on this platform",
        )
    }

    pub(super) fn open_file(_: &fs::File, _: &str) -> io::Result<fs::File> {
        Err(unsupported())
    }

    pub(super) fn open_dir(_: &fs::File, _: &str) -> io::Result<fs::File> {
        Err(unsupported())
    }

    pub(super) fn current_path(_: &fs::File) -> Option<std::path::PathBuf> {
        None
    }

    pub(super) fn random() -> io::Result<[u8; 16]> {
        Err(unsupported())
    }

    pub(super) fn set_attr(_: &fs::File, _: &[u8]) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn get_attr(_: &fs::File) -> io::Result<Option<Vec<u8>>> {
        Err(unsupported())
    }
}
