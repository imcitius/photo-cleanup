//! Physical-disk awareness.
//!
//! Two things depend on knowing which filesystem a path lives on:
//!
//!  * the walk shards its readers per spindle, because an Unraid array is not
//!    striped — each file lives entirely on one disk, and parallel readers pay
//!    off only across disks, not within one;
//!  * quarantine is written to the *same* filesystem as the source, so moving
//!    a bundle is a `rename(2)` instead of a copy.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::volume::device_of;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// Identity of the filesystem: `st_dev` on Unix, the drive or share on
    /// Windows. What everything else keys off.
    pub dev: u64,
    /// Mount point, i.e. the highest ancestor still on the same `st_dev`.
    pub mount: PathBuf,
    /// Short human label: `disk3` for `/mnt/disk3`, `root` for `/`.
    pub label: String,
}

impl Disk {
    /// Path of `src` relative to this disk's mount point, used to mirror the
    /// original layout inside the quarantine tree.
    pub fn relative<'a>(&self, src: &'a Path) -> &'a Path {
        src.strip_prefix(&self.mount).unwrap_or(src)
    }
}

/// Highest ancestor of `path` that still lives on the same filesystem.
pub fn mount_root(path: &Path) -> io::Result<PathBuf> {
    let path = path.canonicalize()?;
    let md = fs::metadata(&path)?;
    let dev = device_of(&md, &path);

    let mut cur = if md.is_dir() {
        path.clone()
    } else {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.clone())
    };

    loop {
        let Some(parent) = cur.parent() else {
            return Ok(cur);
        };
        match fs::metadata(parent) {
            Ok(m) if device_of(&m, parent) == dev => cur = parent.to_path_buf(),
            // A different device, or an unreadable parent: `cur` is the top.
            _ => return Ok(cur),
        }
    }
}

/// `st_dev` of `path`, or of its nearest existing ancestor.
///
/// A destination directory usually does not exist yet — the point of asking
/// is to find out which filesystem it *will* be created on, so that a move
/// into it is a rename and not a copy of the whole archive.
pub fn dev_of_nearest_existing(path: &Path) -> io::Result<u64> {
    let mut cur = path;
    loop {
        if let Ok(md) = fs::metadata(cur) {
            return Ok(device_of(&md, cur));
        }
        cur = cur.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                crate::tf!(
                    "не найти существующий предок для {0}",
                    "cannot find an existing ancestor of {0}",
                    path.display()
                ),
            )
        })?;
    }
}

/// Bytes an unprivileged process may still write on the filesystem of `path`,
/// or of its nearest existing ancestor.
///
/// Asked before copying the app's own data to a new folder: a copy that runs
/// out of room halfway is refused before it starts rather than cleaned up
/// after. It is the space available to this user (`f_bavail`, the caller's
/// quota on Windows), not the total free space — root's reserve is not ours.
pub fn available_space(path: &Path) -> io::Result<u64> {
    let mut cur = path;
    while fs::metadata(cur).is_err() {
        cur = cur.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                crate::tf!(
                    "не найти существующий предок для {0}",
                    "cannot find an existing ancestor of {0}",
                    path.display()
                ),
            )
        })?;
    }
    available_on(cur)
}

#[cfg(unix)]
fn available_on(path: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the
    // call, and `st` is a plain-data struct the call fills in.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    #[allow(clippy::unnecessary_cast)] // the field widths differ by platform
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(windows)]
fn available_on(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0u64;
    // SAFETY: `wide` is NUL-terminated and outlives the call; the two null
    // pointers are the optional totals we do not ask for.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

fn label_for(mount: &Path) -> String {
    match mount.file_name().and_then(|s| s.to_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "root".to_string(),
    }
}

/// Caches the mount lookup, which costs a `stat` per ancestor.
#[derive(Debug, Default)]
pub struct DiskMap {
    by_dev: HashMap<u64, Disk>,
}

impl DiskMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn resolve(&mut self, path: &Path) -> io::Result<Disk> {
        let dev = device_of(&fs::metadata(path)?, path);
        if let Some(d) = self.by_dev.get(&dev) {
            return Ok(d.clone());
        }
        let mount = mount_root(path)?;
        let disk = Disk {
            dev,
            mount: mount.clone(),
            label: label_for(&mount),
        };
        self.by_dev.insert(dev, disk.clone());
        Ok(disk)
    }

    pub fn known(&self) -> impl Iterator<Item = &Disk> {
        self.by_dev.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_and_caches() {
        let tmp = tempfile::tempdir().unwrap();
        let mut map = DiskMap::new();
        let a = map.resolve(tmp.path()).unwrap();
        let b = map.resolve(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(map.known().count(), 1);
    }

    #[test]
    fn relative_strips_the_mount_prefix() {
        let disk = Disk {
            dev: 1,
            mount: PathBuf::from("/mnt/disk3"),
            label: "disk3".into(),
        };
        assert_eq!(
            disk.relative(Path::new("/mnt/disk3/data/foto/X")),
            Path::new("data/foto/X")
        );
    }
}

#[cfg(test)]
mod space_tests {
    use super::available_space;

    #[test]
    fn free_space_is_asked_of_the_nearest_existing_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let here = available_space(tmp.path()).unwrap();
        assert!(here > 0);
        // A folder that does not exist yet is on its parent's filesystem.
        let later = available_space(&tmp.path().join("Новая папка/данные")).unwrap();
        assert!(later > 0);
    }
}

/// Publish a file or directory on the same filesystem without replacing any
/// existing name, including a dangling symlink. A check followed by `rename`
/// is not enough: another process can create the destination in between.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both C strings remain valid throughout the syscall.
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "exclusive rename is unavailable",
        ));
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: paths are NUL terminated; zero flags forbid replacement.
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
