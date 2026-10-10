//! Entries created through a held directory, never by a path looked up
//! again (el-usdqi, el-5vue3 R1/D1–D3).
//!
//! A path names whatever bears the name *now*. Publication that renamed a
//! name published whatever bore it. Here every entry is reached through an
//! open directory descriptor (so its ancestors cannot be swapped
//! underneath), and what was published is compared with the object this
//! process created — device and inode of the open descriptor.
//!
//! Nothing is ever removed through this module (user decision 2026-10-04,
//! el-1y8uo B1). POSIX has no conditional unlink: between any comparison
//! and `unlinkat` another program with write access to the directory can
//! substitute the entry, and the removal would take the stranger's entry
//! instead. An entry this process created and could not publish is left in
//! place and reported by its caller (DESIGN §11.2).

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub use crate::whereabouts::Whereabouts;

/// `(device, inode)` of an object, as `fstat`/`fstatat` report it.
pub type Ident = (u64, u64);

pub fn ident_of(md: &std::fs::Metadata) -> Ident {
    (md.dev(), md.ino())
}

fn cname(name: &str) -> io::Result<CString> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a single directory entry name: {name:?}"),
        ));
    }
    Ok(CString::new(name)?)
}

fn cvt(rc: libc::c_int) -> io::Result<libc::c_int> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

/// An entry as `fstatat(AT_SYMLINK_NOFOLLOW)` reports it ([`Dir::entry_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub ident: Ident,
    /// `st_mode`: the type bits and the permission bits.
    pub mode: u32,
    pub uid: u32,
    pub nlink: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
    }

    pub fn is_file(&self) -> bool {
        self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
    }
}

/// An open directory, and the path it was opened by (for messages only).
pub struct Dir {
    fd: File,
    path: PathBuf,
}

impl Dir {
    /// Open `path` as a directory, refusing a symlink at its last component.
    pub fn open(path: &Path) -> io::Result<Dir> {
        Self::open_with(path, libc::O_NOFOLLOW)
    }

    /// Open `path` as a directory, following a symlink at its last
    /// component — for an ancestor the user named, never for an entry this
    /// tool created.
    pub fn open_following(path: &Path) -> io::Result<Dir> {
        Self::open_with(path, 0)
    }

    fn open_with(path: &Path, extra: libc::c_int) -> io::Result<Dir> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: NUL-terminated path; the descriptor is owned below.
        let fd = cvt(unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | extra,
            )
        })?;
        // SAFETY: a fresh descriptor nobody else owns.
        let fd = unsafe { File::from_raw_fd(fd) };
        Ok(Dir {
            fd,
            path: path.to_path_buf(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn ident(&self) -> io::Result<Ident> {
        Ok(ident_of(&self.fd.metadata()?))
    }

    pub fn file(&self) -> &File {
        &self.fd
    }

    /// What bears `name` in this directory now, without following a link.
    pub fn stat_at(&self, name: &str) -> io::Result<(Ident, u32)> {
        let c = cname(name)?;
        // SAFETY: zero is valid for `stat`; the call fills it.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor, NUL-terminated name, writable buffer.
        cvt(unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        #[allow(clippy::unnecessary_cast)]
        Ok(((st.st_dev as u64, st.st_ino as u64), st.st_mode as u32))
    }

    /// A new file at `name`: fails if anything — a file, a link, a dangling
    /// link — already bears the name.
    pub fn create_new(&self, name: &str, mode: u32) -> io::Result<File> {
        let c = cname(name)?;
        // SAFETY: valid descriptor and name; the descriptor is owned below.
        let fd = cvt(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                mode as libc::c_uint,
            )
        })?;
        // SAFETY: a fresh descriptor nobody else owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Open an existing plain entry at `name` for reading (and writing),
    /// refusing a symlink at the name.
    pub fn open_file(&self, name: &str, write: bool) -> io::Result<File> {
        let c = cname(name)?;
        let access = if write { libc::O_RDWR } else { libc::O_RDONLY };
        // SAFETY: valid descriptor and name; the descriptor is owned below.
        let fd = cvt(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                access | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        })?;
        // SAFETY: a fresh descriptor nobody else owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Make a directory `name` and open it. Its identity is that of the
    /// directory opened right after `mkdirat`; a substitution in between
    /// these two calls is the residual of this module.
    pub fn mkdir(&self, name: &str) -> io::Result<Dir> {
        let c = cname(name)?;
        // SAFETY: valid descriptor and name.
        cvt(unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), 0o755) })?;
        self.open_dir(name)
    }

    /// `mkdirat(fd, name, mode)` without opening it. The umask can only
    /// take bits away from `mode`, never add them.
    pub fn mkdir_mode(&self, name: &str, mode: u32) -> io::Result<()> {
        let c = cname(name)?;
        // SAFETY: valid descriptor and name.
        cvt(unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), mode as libc::mode_t) })?;
        Ok(())
    }

    /// What bears `name` in this folder now, without following a link:
    /// identity, mode (type and permissions), owner and number of names.
    pub fn entry_at(&self, name: &str) -> io::Result<Entry> {
        let c = cname(name)?;
        // SAFETY: zero is valid for `stat`; the call fills it.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: valid descriptor, NUL-terminated name, writable buffer.
        cvt(unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        #[allow(clippy::unnecessary_cast)]
        Ok(Entry {
            ident: (st.st_dev as u64, st.st_ino as u64),
            mode: st.st_mode as u32,
            uid: st.st_uid as u32,
            nlink: st.st_nlink as u64,
        })
    }

    pub fn open_dir(&self, name: &str) -> io::Result<Dir> {
        let c = cname(name)?;
        // SAFETY: valid descriptor and name; the descriptor is owned below.
        let fd = cvt(unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        Ok(Dir {
            // SAFETY: a fresh descriptor nobody else owns.
            fd: unsafe { File::from_raw_fd(fd) },
            path: self.path.join(name),
        })
    }

    /// Rename `from` to `to`, both in this directory, never replacing.
    pub fn rename_no_replace(&self, from: &str, to: &str) -> io::Result<()> {
        let (a, b) = (cname(from)?, cname(to)?);
        crate::disk::rename_no_replace_at(&self.fd, &a, &self.fd, &b)
    }

    /// `unlinkat(fd, name, 0)`: the entry `name` in *this* folder, never a
    /// path. POSIX has no conditional unlink — whatever bears the name at the
    /// moment of the call goes — so the caller compares first and, after,
    /// reads what it held to say what actually went (el-3s9kp).
    pub fn remove_file_at(&self, name: &str) -> io::Result<()> {
        let c = cname(name)?;
        cvt(unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) })?;
        Ok(())
    }

    /// `unlinkat(fd, name, AT_REMOVEDIR)`: an *empty* folder in this folder.
    /// The system refuses a folder with anything in it; nothing here walks
    /// into one.
    pub fn remove_dir_at(&self, name: &str) -> io::Result<()> {
        let c = cname(name)?;
        cvt(unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) })?;
        Ok(())
    }

    /// The names in this folder (without `.` and `..`), read through the
    /// held descriptor, not through its path.
    pub fn names(&self) -> io::Result<Vec<std::ffi::OsString>> {
        // `fdopendir` takes the descriptor over; it gets its own duplicate,
        // rewound, so the held one is neither closed nor moved.
        let dup = cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
        let dir = unsafe { libc::fdopendir(dup) };
        if dir.is_null() {
            let e = io::Error::last_os_error();
            unsafe { libc::close(dup) };
            return Err(e);
        }
        unsafe { libc::rewinddir(dir) };
        let mut out = Vec::new();
        let result = loop {
            // readdir reports an error only through errno, so it is cleared
            // first: a null with errno still 0 is the end of the folder.
            #[cfg(target_os = "macos")]
            unsafe {
                *libc::__error() = 0
            };
            #[cfg(target_os = "linux")]
            unsafe {
                *libc::__errno_location() = 0
            };
            let ent = unsafe { libc::readdir(dir) };
            if ent.is_null() {
                let e = io::Error::last_os_error();
                break match e.raw_os_error() {
                    Some(0) | None => Ok(()),
                    _ => Err(e),
                };
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()) };
            let bytes = name.to_bytes();
            if bytes != b"." && bytes != b".." {
                out.push(std::ffi::OsStr::from_bytes(bytes).to_os_string());
            }
        };
        unsafe { libc::closedir(dir) };
        result.map(|()| out)
    }

    pub fn sync(&self) -> io::Result<()> {
        self.fd.sync_all()
    }

    /// Where the folder held is now, as the system names it from the
    /// descriptor (`F_GETPATH` on macOS, `/proc/self/fd` on Linux); `None`
    /// where the system does not say. Nothing is searched.
    pub fn current_path(&self) -> Option<PathBuf> {
        current_path(&self.fd)
    }

    /// The same folder, held by a second descriptor.
    pub fn try_clone(&self) -> io::Result<Dir> {
        Ok(Dir {
            fd: self.fd.try_clone()?,
            path: self.path.clone(),
        })
    }

    /// `path` leads to this folder now (following links on the way, as the
    /// user's own path would).
    pub fn is_at(&self, path: &Path) -> bool {
        match (std::fs::metadata(path), self.ident()) {
            (Ok(now), Ok(held)) => ident_of(&now) == held,
            _ => false,
        }
    }
}

/// Where an open file is now, as the system names it from the descriptor;
/// `None` where it does not say. Unverified: see [`locate`].
pub fn current_path_of(file: &File) -> Option<PathBuf> {
    current_path(file)
}

/// Where the object `obj` is, proven (el-lvtmk §3.1).
///
/// `dir` is the folder held for it and `name` the name it was given there;
/// `recorded` is the full path the caller would state. `Verified` needs all
/// of: the name in the held folder bears `obj`; a path to that folder — the
/// recorded one first, else the one the system names from the descriptor —
/// leads to the held folder now; and that path joined with the name bears
/// `obj`. If the object is not in the folder, the open file `file` (when
/// there is one) says whether it still has a name at all (`Unlinked`) and
/// where the system thinks it is, which is proven the same way.
///
/// Nothing is searched. Anything short of proof is `Uncertain`, with the
/// last place seen, unverified.
pub fn locate(
    dir: &Dir,
    name: &str,
    obj: Ident,
    recorded: Option<&Path>,
    file: Option<&File>,
) -> Whereabouts {
    let bears = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| ident_of(&m) == obj);
    if matches!(dir.stat_at(name), Ok((id, _)) if id == obj) {
        let mut tried = Vec::new();
        if let Some(r) = recorded {
            if r.file_name().and_then(|n| n.to_str()) == Some(name) {
                tried.push(r.to_path_buf());
            }
        }
        tried.push(dir.join(name));
        let now = dir.current_path().map(|p| p.join(name));
        if let Some(p) = &now {
            tried.push(p.clone());
        }
        for p in &tried {
            let folder = p.parent().unwrap_or(Path::new("."));
            if dir.is_at(folder) && bears(p) {
                return Whereabouts::verified(p.clone());
            }
        }
        return Whereabouts::uncertain(
            now.or_else(|| Some(dir.join(name))),
            crate::tr!(
                "папку, где он лежит, нельзя назвать путём, который туда ведёт",
                "the folder it is in cannot be named by a path that leads there"
            ),
        );
    }
    let Some(f) = file else {
        return Whereabouts::uncertain(
            None,
            crate::tf!(
                "под именем {0} в его папке его больше нет",
                "it no longer bears the name {0} in its folder",
                name
            ),
        );
    };
    match f.metadata() {
        Ok(md) if ident_of(&md) == obj && std::os::unix::fs::MetadataExt::nlink(&md) == 0 => {
            return Whereabouts::Unlinked
        }
        _ => {}
    }
    match current_path(f) {
        Some(p) if bears(&p) => Whereabouts::verified(p),
        Some(p) => Whereabouts::uncertain(
            Some(p),
            crate::tr!(
                "система называет это место, но под этим именем уже не он",
                "the system names this place, but the name no longer bears it"
            ),
        ),
        None => Whereabouts::uncertain(
            None,
            crate::tr!(
                "его нет в его папке, а система не говорит, где он",
                "it is not in its folder, and the system does not say where it is"
            ),
        ),
    }
}

#[cfg(target_os = "macos")]
fn current_path(file: &File) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN bytes into `buf`.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0)?;
    Some(std::ffi::OsStr::from_bytes(&buf[..end]).into())
}

#[cfg(target_os = "linux")]
fn current_path(file: &File) -> Option<PathBuf> {
    let path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    // A removed folder is shown with this suffix; it has no place then.
    (!path.to_string_lossy().ends_with(" (deleted)")).then_some(path)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn current_path(_: &File) -> Option<PathBuf> {
    None
}

/// Tests only: what another program does at a boundary of this module.
#[cfg(test)]
pub(crate) mod seam {
    use std::cell::RefCell;
    use std::path::Path;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Stage {
        /// Before a new file is created at the path.
        Create,
        /// After the file is written, before it is synced.
        Written,
        /// Before the temporary at the path is published.
        Publish,
    }

    type Hook = Box<dyn FnMut(Stage, &Path) -> std::io::Result<()>>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    pub(crate) struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|h| *h.borrow_mut() = None);
        }
    }

    pub(crate) fn set(hook: impl FnMut(Stage, &Path) -> std::io::Result<()> + 'static) -> Guard {
        HOOK.with(|h| *h.borrow_mut() = Some(Box::new(hook)));
        Guard
    }

    /// Run the hook; its error is returned to the caller as the call's.
    pub(crate) fn fire_result(stage: Stage, path: &Path) -> std::io::Result<()> {
        HOOK.with(|h| match h.borrow_mut().as_mut() {
            Some(hook) => hook(stage, path),
            None => Ok(()),
        })
    }
}
