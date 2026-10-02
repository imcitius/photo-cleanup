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

/// Move a file or directory within one filesystem without replacing any
/// existing name — a file, a directory (even an empty one) or a dangling
/// symlink. A check followed by `rename` is not enough: another program can
/// create the destination in between, and a plain `rename` replaces it
/// without a word (el-usdqi).
///
/// - macOS: `renameatx_np(RENAME_EXCL)`;
/// - Linux: `renameat2(RENAME_NOREPLACE)`;
/// - Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (and
///   without `MOVEFILE_COPY_ALLOWED`, so it never becomes a copy);
/// - other systems have no such call and refuse.
///
/// An existing destination fails with [`io::ErrorKind::AlreadyExists`]. A
/// volume that lacks the call fails as [`lacks_exclusive_rename`] says; there
/// is deliberately no fallback — every substitute (check then rename, a
/// placeholder, link then unlink) reopens the window or leaves debris.
pub fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        exclusive::rename(libc::AT_FDCWD, &from, libc::AT_FDCWD, &to)
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

/// [`rename_no_replace`] relative to open directories: `from` in `from_dir`
/// to `to` in `to_dir`. The one implementation behind both forms.
#[cfg(unix)]
pub fn rename_no_replace_at(
    from_dir: &fs::File,
    from: &std::ffi::CStr,
    to_dir: &fs::File,
    to: &std::ffi::CStr,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    exclusive::rename(from_dir.as_raw_fd(), from, to_dir.as_raw_fd(), to)
}

#[cfg(unix)]
mod exclusive {
    use std::ffi::CStr;
    use std::io;
    use std::os::fd::RawFd;

    pub(super) fn rename(a: RawFd, from: &CStr, b: RawFd, to: &CStr) -> io::Result<()> {
        // SAFETY: descriptors (or AT_FDCWD) and NUL-terminated names that
        // outlive the call.
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renameatx_np(a, from.as_ptr(), b, to.as_ptr(), libc::RENAME_EXCL) };
        // SAFETY: as above.
        #[cfg(target_os = "linux")]
        let rc =
            unsafe { libc::renameat2(a, from.as_ptr(), b, to.as_ptr(), libc::RENAME_NOREPLACE) };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (a, b, from, to);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exclusive rename is unavailable",
            ));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// Whether `e`, returned by [`rename_no_replace`], means the volume (or the
/// system) cannot rename without replacing — not that this one entry could
/// not move. exFAT on macOS answers `ENOTSUP`; Linux answers `EINVAL` where
/// the file system lacks `RENAME_NOREPLACE` (NFS, many FUSE mounts).
pub fn lacks_exclusive_rename(e: &io::Error) -> bool {
    if e.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        let code = e.raw_os_error();
        if code == Some(libc::ENOTSUP) || code == Some(libc::EOPNOTSUPP) {
            return true;
        }
        #[cfg(target_os = "linux")]
        if code == Some(libc::EINVAL) {
            return true;
        }
    }
    false
}

/// What a volume says about renaming without replacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusiveRename {
    /// The volume reports the capability.
    Supported,
    /// The volume reports that it lacks it (macOS exFAT).
    Absent,
    /// The volume does not say either way: not proven, treat as absent.
    Unreported,
    /// This system has no such query (Linux, Windows): only the call
    /// itself answers, and it moves nothing when it refuses.
    NoQuery,
}

/// Ask the volume that holds the existing `path` whether it can rename
/// without replacing. macOS: `VOL_CAP_INT_RENAME_EXCL` of
/// `ATTR_VOL_CAPABILITIES` (el-21zyg: exFAT reports it absent, APFS and
/// HFS+ report it). Read only.
pub fn exclusive_rename(path: &Path) -> io::Result<ExclusiveRename> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        /// `ATTR_VOL_CAPABILITIES` as `getattrlist` returns it: a length,
        /// then the attribute.
        #[repr(C, packed(4))]
        struct Capabilities {
            length: u32,
            caps: libc::vol_capabilities_attr_t,
        }

        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: plain data; zero is a valid value for every field.
        let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
        list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        list.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES;
        let mut out: Capabilities = unsafe { std::mem::zeroed() };
        // SAFETY: `c` is NUL-terminated; `out` is writable for its size.
        let rc = unsafe {
            libc::getattrlist(
                c.as_ptr(),
                (&mut list as *mut libc::attrlist).cast(),
                (&mut out as *mut Capabilities).cast(),
                std::mem::size_of::<Capabilities>(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let caps = out.caps;
        let i = libc::VOL_CAPABILITIES_INTERFACES;
        let bit = libc::VOL_CAP_INT_RENAME_EXCL;
        Ok(
            match (caps.valid[i] & bit != 0, caps.capabilities[i] & bit != 0) {
                (true, true) => ExclusiveRename::Supported,
                (true, false) => ExclusiveRename::Absent,
                (false, _) => ExclusiveRename::Unreported,
            },
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        fs::symlink_metadata(path)?;
        Ok(ExclusiveRename::NoQuery)
    }
}

#[cfg(test)]
mod exclusive_tests {
    use super::*;

    #[test]
    fn nothing_at_the_new_name_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, b"mine").unwrap();
        fs::write(&b, b"theirs").unwrap();
        let e = rename_no_replace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert!(!lacks_exclusive_rename(&e));
        assert_eq!(fs::read(&a).unwrap(), b"mine");
        assert_eq!(fs::read(&b).unwrap(), b"theirs");

        // A directory onto an empty directory: plain `rename` replaces it.
        let (d, e_) = (tmp.path().join("d"), tmp.path().join("e"));
        fs::create_dir(&d).unwrap();
        fs::create_dir(&e_).unwrap();
        assert!(rename_no_replace(&d, &e_).is_err());
        assert!(d.is_dir() && e_.is_dir());

        let c = tmp.path().join("c");
        rename_no_replace(&a, &c).unwrap();
        assert_eq!(fs::read(&c).unwrap(), b"mine");
        assert!(!a.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_not_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        fs::write(&a, b"mine").unwrap();
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &b).unwrap();
        assert!(!b.exists(), "exists() does not see it — that is the point");
        let e = rename_no_replace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");
        assert!(fs::symlink_metadata(&b).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&a).unwrap(), b"mine");
    }

    #[test]
    fn an_ordinary_temporary_folder_can_rename_without_replacing() {
        let tmp = tempfile::tempdir().unwrap();
        let answer = exclusive_rename(tmp.path()).unwrap();
        assert!(
            matches!(
                answer,
                ExclusiveRename::Supported | ExclusiveRename::NoQuery
            ),
            "{answer:?}"
        );
    }
}
