//! Whether the volume that would hold a relocation target can keep this
//! run's temporary folders private to this user (el-2xri; Director decision
//! A on review el-6d0i0).
//!
//! The staging folders and the private cleanup folders are safe only
//! because nobody but this user (and root/administrator, outside the threat
//! model) can put anything into them or rename them. That rests on the
//! volume itself: it has to record and enforce file owners, and it has to
//! accept the access list the folders are created with
//! ([`super::create_private_dir`]). Where it does not, a relocation to it is
//! refused up front — in the preview and again before the first write —
//! instead of being attempted with weaker folders:
//!
//! - macOS: a volume mounted with `MNT_IGNORE_OWNERSHIP` ("noowners"; FAT32
//!   and exFAT always, external HFS+/APFS by default) shows every entry as
//!   owned by whoever looks, so every local user has owner rights there.
//!   A volume without extended security (`_PC_EXTENDED_SECURITY_NP`) cannot
//!   take the no-inherit ACL `mkdirx_np` creates the folders with.
//! - Windows: a volume without `FILE_PERSISTENT_ACLS` (FAT32, exFAT) keeps
//!   neither owner nor DACL, so the protected owner-only DACL cannot exist.
//! - Linux (not a desktop target; the library is tested there): only file
//!   systems known to store POSIX owners and modes are accepted, by
//!   `statfs` magic; anything else (vfat, exfat, fuse, network) is refused.
//! - Other platforms: refused, nothing is known.
//!
//! The answer is read from the volume, not assumed from its type: the same
//! APFS is accepted on the system disk and refused on a noowners mount.

use std::fs;
use std::path::{Path, PathBuf};

/// What a volume says about itself, as far as private folders are concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Volume {
    /// File system name (`apfs`, `msdos`, `NTFS`, `ext4`, ...).
    pub(crate) filesystem: String,
    /// Where it is mounted, when the platform says.
    pub(crate) mount: Option<PathBuf>,
    /// The file system is one whose owners and permissions are known to be
    /// stored and enforced (always true where the platform reports
    /// ownership handling itself, as macOS and Windows do).
    pub(crate) known: bool,
    /// Mounted so that ownership is ignored (macOS `noowners`).
    pub(crate) ignores_owners: bool,
    /// Supports the access lists the private folders are created with
    /// (macOS extended security, Windows persistent ACLs; Linux needs none).
    pub(crate) acls: bool,
}

impl Volume {
    fn describe(&self) -> String {
        match &self.mount {
            Some(mount) => format!("{} ({})", mount.display(), self.filesystem),
            None => self.filesystem.clone(),
        }
    }
}

/// Why folders made on `volume` could not be kept private to this user.
pub(crate) fn verdict(volume: &Volume) -> Result<(), String> {
    let name = volume.describe();
    if !volume.known {
        return Err(pc_core::tf!(
            "файловая система тома {0} не входит в число проверенных, где хранятся \
             владельцы файлов и права доступа",
            "the file system of volume {0} is not one known to store file owners and \
             permissions",
            name
        ));
    }
    if volume.ignores_owners {
        return Err(pc_core::tf!(
            "том {0} подключён без учёта владельцев (noowners): на нём любой пользователь \
             этого компьютера получает права владельца на любую папку",
            "volume {0} is mounted with ownership ignored (noowners): every user of this \
             computer has owner rights to every folder on it",
            name
        ));
    }
    if !volume.acls {
        return Err(pc_core::tf!(
            "файловая система тома {0} не хранит права доступа (ACL), без них папку \
             нельзя создать закрытой от других пользователей",
            "the file system of volume {0} does not store access lists (ACLs), without \
             which a folder cannot be created private to this user",
            name
        ));
    }
    Ok(())
}

/// Check the volume that holds `target`, or would hold it once created:
/// that of its nearest existing ancestor. `Err` is the reason, in words.
pub(crate) fn check_target(target: &Path) -> Result<(), String> {
    let existing = nearest_existing(target)?;
    let volume = probe_path(existing).map_err(|e| {
        pc_core::tf!(
            "не узнать свойства тома, на котором {0}: {1}",
            "cannot read the properties of the volume holding {0}: {1}",
            existing.display(),
            e
        )
    })?;
    verdict(&volume)
}

/// The nearest existing folder of `target` (itself, if it exists).
fn nearest_existing(target: &Path) -> Result<&Path, String> {
    let mut existing = target;
    while fs::metadata(existing).is_err() {
        existing = existing.parent().ok_or_else(|| {
            pc_core::tf!(
                "не найти существующую папку, в которой будет {0}",
                "cannot find an existing folder that would hold {0}",
                target.display()
            )
        })?;
    }
    Ok(existing)
}

/// Whether the volume that holds `target` (or would hold it) says it can
/// rename an entry *without replacing* whatever is at the new name
/// (el-21zyg). Every move of this run's objects — publication and the
/// cleanup's move into a private folder — depends on that call; there is
/// no safe substitute for it (a plain rename can replace someone's entry,
/// and "check the name, then rename" cannot exclude that either). Read
/// only; `Err` is the reason, naming the volume.
///
/// - macOS: the volume's `VOL_CAP_INT_RENAME_EXCL` capability. exFAT
///   reports it absent and fails `renameatx_np(RENAME_EXCL)` with `ENOTSUP`
///   (45); APFS and HFS+ report it. A capability not reported at all is
///   not proven and is refused too.
/// - Elsewhere there is no such query: Linux answers only by trying
///   (`renameat2(RENAME_NOREPLACE)` fails with `EINVAL` where unsupported),
///   which the move does before it writes anything it would have to clean
///   up ([`super::probe_exclusive_rename`]); Windows renames through handles
///   and does not copy at all ([`crate::namespace`]).
pub(crate) fn check_exclusive_rename(target: &Path) -> Result<(), String> {
    let existing = nearest_existing(target)?;
    platform::exclusive_rename(existing).map_err(|e| {
        pc_core::tf!(
            "не узнать, умеет ли том, на котором {0}, переименовывать без замены: {1}",
            "cannot tell whether the volume holding {0} can rename without replacing: {1}",
            existing.display(),
            e
        )
    })?
}

/// The same check through an open handle: for a folder just created, so
/// the answer is about the volume it is actually on.
pub(crate) fn check_open(file: &fs::File) -> Result<(), String> {
    let volume = probe_file(file)
        .map_err(|e| format!("the properties of its volume cannot be read: {e}"))?;
    verdict(&volume).map_err(|why| format!("it cannot be private there: {why}"))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::Volume;
    use std::ffi::{CStr, CString};
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::{fs, io};

    // <sys/mount.h>, <sys/unistd.h>
    const MNT_IGNORE_OWNERSHIP: u32 = 0x0020_0000;
    const MNT_LOCAL: u32 = 0x0000_1000;
    /// Local file systems known to store owners, modes and ACLs and to
    /// enforce them. Anything else — network shares (the server decides,
    /// not this computer), FUSE, FAT, exFAT, NTFS — is not proven.
    const OWNED: [&str; 2] = ["apfs", "hfs"];
    const PC_EXTENDED_SECURITY_NP: libc::c_int = 13;

    fn volume(st: &libc::statfs, extended_security: libc::c_long) -> Volume {
        // SAFETY: the kernel NUL-terminates both fixed-size fields.
        let text = |field: &[libc::c_char]| unsafe { CStr::from_ptr(field.as_ptr()) };
        let filesystem = text(&st.f_fstypename).to_string_lossy().into_owned();
        Volume {
            known: st.f_flags & MNT_LOCAL != 0 && OWNED.contains(&filesystem.as_str()),
            filesystem,
            mount: Some(PathBuf::from(std::ffi::OsStr::from_bytes(
                text(&st.f_mntonname).to_bytes(),
            ))),
            ignores_owners: st.f_flags & MNT_IGNORE_OWNERSHIP != 0,
            acls: extended_security == 1,
        }
    }

    pub(crate) fn probe_path(path: &Path) -> io::Result<Volume> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `c` is NUL-terminated and outlives both calls; `st` is
        // plain data the call fills in.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // -1 (unsupported name or error) counts as "no ACLs".
        let acl = unsafe { libc::pathconf(c.as_ptr(), PC_EXTENDED_SECURITY_NP) };
        Ok(volume(&st, acl))
    }

    pub(crate) fn probe_file(file: &fs::File) -> io::Result<Volume> {
        // SAFETY: `file` owns a valid descriptor; `st` is plain data.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(file.as_raw_fd(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let acl = unsafe { libc::fpathconf(file.as_raw_fd(), PC_EXTENDED_SECURITY_NP) };
        Ok(volume(&st, acl))
    }

    /// `ATTR_VOL_CAPABILITIES` as `getattrlist` returns it: a length, then
    /// the attribute.
    #[repr(C, packed(4))]
    struct Capabilities {
        length: u32,
        caps: libc::vol_capabilities_attr_t,
    }

    /// `Ok(Err(reason))`: the volume is known not to (or does not say it
    /// can) rename without replacing.
    pub(crate) fn exclusive_rename(path: &Path) -> io::Result<Result<(), String>> {
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
        let (valid, set) = (caps.valid[i] & bit != 0, caps.capabilities[i] & bit != 0);
        if valid && set {
            return Ok(Ok(()));
        }
        let name = probe_path(path).map_or_else(|_| path.display().to_string(), |v| v.describe());
        Ok(Err(if valid {
            pc_core::tf!(
                "том {0} не умеет переименовывать без замены существующего (RENAME_EXCL)",
                "volume {0} cannot rename without replacing what is at the new name \
                 (RENAME_EXCL)",
                name
            )
        } else {
            pc_core::tf!(
                "том {0} не сообщает, умеет ли он переименовывать без замены (RENAME_EXCL)",
                "volume {0} does not say whether it can rename without replacing (RENAME_EXCL)",
                name
            )
        }))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::Volume;
    use std::ffi::CString;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::{fs, io};

    /// `statfs` magics of file systems that store POSIX owners and modes
    /// and enforce them for every user (<linux/magic.h>).
    const OWNED: [(u64, &str); 12] = [
        (0xEF53, "ext2/3/4"),
        (0x5846_5342, "xfs"),
        (0x9123_683E, "btrfs"),
        (0x0102_1994, "tmpfs"),
        (0x8584_58F6, "ramfs"),
        (0x794C_7630, "overlayfs"),
        (0x2FC1_2FC1, "zfs"),
        (0xF2F5_2010, "f2fs"),
        (0xCA45_1A4E, "bcachefs"),
        (0x3153_464A, "jfs"),
        (0x5265_4973, "reiserfs"),
        (0x3434, "nilfs"),
    ];

    pub(crate) fn from_magic(magic: u64) -> Volume {
        let magic = magic & 0xFFFF_FFFF;
        let found = OWNED.iter().find(|(m, _)| *m == magic);
        Volume {
            filesystem: found.map_or_else(|| format!("statfs 0x{magic:x}"), |(_, n)| n.to_string()),
            mount: None,
            known: found.is_some(),
            ignores_owners: false,
            acls: true,
        }
    }

    #[allow(clippy::unnecessary_cast)] // `f_type`'s width differs by libc
    fn magic(st: &libc::statfs) -> u64 {
        st.f_type as u64
    }

    pub(crate) fn probe_path(path: &Path) -> io::Result<Volume> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: `c` is NUL-terminated for the call; `st` is plain data.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(from_magic(magic(&st)))
    }

    pub(crate) fn probe_file(file: &fs::File) -> io::Result<Volume> {
        // SAFETY: `file` owns a valid descriptor; `st` is plain data.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(file.as_raw_fd(), &mut st) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(from_magic(magic(&st)))
    }

    /// No capability query here; see [`super::check_exclusive_rename`].
    pub(crate) fn exclusive_rename(_: &Path) -> io::Result<Result<(), String>> {
        Ok(Ok(()))
    }
}

#[cfg(windows)]
mod platform {
    use super::Volume;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::{Path, PathBuf};
    use std::{fs, io, iter, ptr};
    use windows_sys::Win32::Storage::FileSystem::{
        GetVolumeInformationByHandleW, GetVolumeInformationW, GetVolumePathNameW,
    };

    // <winnt.h>
    const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;
    const LEN: usize = 261;

    fn text(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    fn volume(flags: u32, name: &[u16], mount: Option<PathBuf>) -> Volume {
        Volume {
            filesystem: text(name),
            mount,
            known: true,
            ignores_owners: false,
            acls: flags & FILE_PERSISTENT_ACLS != 0,
        }
    }

    pub(crate) fn probe_path(path: &Path) -> io::Result<Volume> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect();
        let mut root = [0u16; LEN];
        let (mut flags, mut name) = (0u32, [0u16; LEN]);
        // SAFETY: every buffer is valid for the length passed with it.
        let ok = unsafe {
            GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), LEN as u32) != 0
                && GetVolumeInformationW(
                    root.as_ptr(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut flags,
                    name.as_mut_ptr(),
                    LEN as u32,
                ) != 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
        Ok(volume(flags, &name, Some(PathBuf::from(text(&root)))))
    }

    pub(crate) fn probe_file(file: &fs::File) -> io::Result<Volume> {
        let (mut flags, mut name) = (0u32, [0u16; LEN]);
        // SAFETY: `file` owns a valid handle; the buffer is valid for LEN.
        let ok = unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut flags,
                name.as_mut_ptr(),
                LEN as u32,
            )
        } != 0;
        if !ok {
            return Err(io::Error::last_os_error());
        }
        Ok(volume(flags, &name, None))
    }

    /// No capability query here; see [`super::check_exclusive_rename`].
    pub(crate) fn exclusive_rename(_: &Path) -> io::Result<Result<(), String>> {
        Ok(Ok(()))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod platform {
    use super::Volume;
    use std::path::Path;
    use std::{fs, io};

    fn unknown() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "volume ownership handling is not known on this platform",
        )
    }

    pub(crate) fn probe_path(_: &Path) -> io::Result<Volume> {
        Err(unknown())
    }

    pub(crate) fn probe_file(_: &fs::File) -> io::Result<Volume> {
        Err(unknown())
    }

    /// No capability query here; see [`super::check_exclusive_rename`].
    pub(crate) fn exclusive_rename(_: &Path) -> io::Result<Result<(), String>> {
        Ok(Ok(()))
    }
}

pub(crate) use platform::probe_file;
use platform::probe_path;

#[cfg(test)]
mod tests {
    use super::*;

    fn apfs() -> Volume {
        Volume {
            filesystem: "apfs".into(),
            mount: Some("/Volumes/Card".into()),
            known: true,
            ignores_owners: false,
            acls: true,
        }
    }

    #[test]
    fn each_missing_capability_is_refused_and_named() {
        assert_eq!(verdict(&apfs()), Ok(()));
        let noowners = verdict(&Volume {
            ignores_owners: true,
            ..apfs()
        })
        .unwrap_err();
        assert!(noowners.contains("/Volumes/Card (apfs)"), "{noowners}");
        assert!(noowners.contains("noowners"), "{noowners}");
        let fat = verdict(&Volume {
            filesystem: "msdos".into(),
            ignores_owners: true,
            acls: false,
            ..apfs()
        })
        .unwrap_err();
        assert!(fat.contains("(msdos)"), "{fat}");
        let no_acl = verdict(&Volume {
            acls: false,
            ..apfs()
        })
        .unwrap_err();
        assert!(no_acl.contains("ACL"), "{no_acl}");
        let unknown = verdict(&Volume {
            known: false,
            mount: None,
            filesystem: "statfs 0x4d44".into(),
            ..apfs()
        })
        .unwrap_err();
        assert!(unknown.contains("statfs 0x4d44"), "{unknown}");
    }

    /// The folder the tests run in is on an ordinary local volume.
    #[test]
    fn a_missing_target_is_judged_by_its_nearest_existing_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("not/yet/there");
        assert_eq!(check_target(&target), Ok(()));
        assert_eq!(check_exclusive_rename(&target), Ok(()));
        assert!(!tmp.path().join("not").exists());
        let dir = fs::File::open(tmp.path()).unwrap();
        assert_eq!(check_open(&dir), Ok(()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_accepts_only_file_systems_that_keep_owners() {
        assert!(platform::from_magic(0xEF53).known);
        assert!(platform::from_magic(0x794C_7630).known);
        // vfat, exfat, fuse, cifs, nfs
        for magic in [0x4D44, 0x2011_BAB0, 0x6573_5546, 0xFF53_4D42, 0x6969] {
            let volume = platform::from_magic(magic);
            assert!(verdict(&volume).is_err(), "{magic:x}");
        }
        // A negative `f_type` from a 32-bit libc is the same magic.
        assert!(platform::from_magic(0xFFFF_FFFF_9123_683E).known);
    }
}
