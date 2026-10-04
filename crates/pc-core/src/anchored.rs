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

    pub fn sync(&self) -> io::Result<()> {
        self.fd.sync_all()
    }
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
