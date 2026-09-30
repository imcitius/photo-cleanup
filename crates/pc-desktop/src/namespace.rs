//! Protected namespaces: folders whose *path* nobody else can redirect.
//!
//! Relocation and the bootstrap work with paths — SQLite takes one, the
//! server takes one, the bootstrap stores one. An open descriptor keeps an
//! object, not its name: anyone allowed to rename entries in any folder on
//! the way can make the same path lead somewhere else between two calls
//! (reviews el-2ztq8, el-59w6z; diagnosis el-49o3y). Checking the name
//! again before each use does not help, because the next rename can come
//! right after the check.
//!
//! So the guarantee is made where it can be made: a path is *admitted* only
//! if nobody but this user and the system administrator can change any
//! folder on it. Then no other account can rename, replace or re-permission
//! anything on the way for as long as that holds, and a path used later
//! leads to the same objects that were checked. The proof, for every folder
//! from `/` down to the deepest existing one, read through descriptors
//! opened one level at a time without following links:
//!
//! - it belongs to root or to this user — nobody else can change its
//!   permissions or access list;
//! - no group or other write bit — nobody else may add, rename or remove
//!   entries in it. The one exception is a sticky folder *above* the last
//!   one (`/tmp`): there others may add entries of their own, but may not
//!   rename or remove the next folder on our path, which belongs to root or
//!   to us;
//! - no access list entry allowing anybody to add, remove or rename
//!   entries, delete the folder, or change its permissions or owner
//!   (macOS extended ACLs; on Linux any POSIX ACL at all);
//! - its volume stores owners and permissions and honours them (local
//!   APFS/HFS+ with ownership on; the Linux file systems in
//!   [`crate::relocate`]'s volume table). A volume that ignores owners
//!   reports every folder as ours.
//!
//! The chain starts at `/`, which belongs to root: the system administrator
//! and processes running as this user have full authority over these files
//! anyway and are outside the product's threat model (DESKTOP.md). Links on
//! the requested path are resolved once, before the walk
//! (`fs::canonicalize`), and the walk itself refuses any link; every later
//! use goes through the canonical path, so a link's target is part of the
//! proof, not a second resolution.
//!
//! What the retained descriptors add: [`Protected::recheck`] walks the path
//! again and compares every folder's identity with the one proven, and
//! repeats the permission checks, so the user's own programs changing
//! something (a `chmod`, a move in Finder) between the admission and a
//! later step is noticed there. They do not by themselves stop a rename —
//! the proof above is what does.
//!
//! Everything that cannot be proven is refused, before anything is written,
//! with the path, the folder that failed and why. That includes every
//! platform without an implementation of this proof (Windows: DACLs of
//! every ancestor, reparse points and sharing modes are not proven here).

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Why a folder is not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The folder that was asked about.
    pub path: PathBuf,
    /// The folder on its way that failed.
    pub component: PathBuf,
    pub reason: String,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.component == self.path {
            write!(f, "{}: {}", self.path.display(), self.reason)
        } else {
            write!(
                f,
                "{}: {} {}",
                self.path.display(),
                self.component.display(),
                self.reason
            )
        }
    }
}

/// One proven folder on the chain.
#[derive(Debug)]
struct Link {
    path: PathBuf,
    dir: fs::File,
    id: (u64, u64),
}

/// A path whose every existing folder is proven to be changeable by this
/// user and root only, with those folders held open. Folders that do not
/// exist yet are created by [`Protected::create_missing`], each checked the
/// same way before the next.
#[derive(Debug)]
pub struct Protected {
    requested: PathBuf,
    /// The canonical path, including the part still to be created.
    path: PathBuf,
    chain: Vec<Link>,
    missing: Vec<OsString>,
    /// How many of the last links this value created.
    created: usize,
}

impl Protected {
    /// The canonical path everything later must use.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder itself, open, once it exists.
    pub fn dir(&self) -> Option<&fs::File> {
        if self.missing.is_empty() {
            self.chain.last().map(|l| &l.dir)
        } else {
            None
        }
    }

    /// The deepest existing folder: its path and open descriptor.
    pub fn deepest(&self) -> (&Path, &fs::File) {
        let link = self.chain.last().expect("the chain starts at /");
        (&link.path, &link.dir)
    }

    fn refuse(&self, component: &Path, reason: impl Into<String>) -> Refusal {
        Refusal {
            path: self.requested.clone(),
            component: component.to_path_buf(),
            reason: reason.into(),
        }
    }

    /// Walk the path again from `/`: every folder must still be the one
    /// proven, and still pass the checks.
    pub fn recheck(&self) -> Result<(), Refusal> {
        let mut current: Option<fs::File> = None;
        let last = self.chain.len().saturating_sub(1);
        for (i, link) in self.chain.iter().enumerate() {
            let opened = match &current {
                None => platform::open_root(),
                Some(parent) => {
                    let name = link.path.file_name().unwrap_or_default();
                    platform::open_child(parent, name)
                }
            };
            let dir = opened.map_err(|e| {
                self.refuse(
                    &link.path,
                    format!("can no longer be opened as the same folder: {e}"),
                )
            })?;
            let id =
                platform::identity(&dir).map_err(|e| self.refuse(&link.path, e.to_string()))?;
            if id != link.id {
                return Err(self.refuse(
                    &link.path,
                    pc_core::tr!(
                        "подменена или переименована после проверки",
                        "was replaced or renamed after the check"
                    ),
                ));
            }
            platform::check(&dir, i == last).map_err(|why| self.refuse(&link.path, why))?;
            current = Some(dir);
        }
        Ok(())
    }

    /// Create the folders that do not exist yet, one level at a time,
    /// relative to the open parent, private to this user (mode 0700), and
    /// check each like the rest of the chain before going on. An entry that
    /// appeared at one of the names meanwhile fails the creation and is left
    /// alone.
    pub fn create_missing(&mut self) -> Result<(), Refusal> {
        while !self.missing.is_empty() {
            let name = self.missing.remove(0);
            let parent = self.chain.last().expect("the chain starts at /");
            let path = parent.path.join(&name);
            platform::mkdir(&parent.dir, &name).map_err(|e| {
                self.refuse(
                    &path,
                    pc_core::tf!("не создать папку: {0}", "cannot create the folder: {0}", e),
                )
            })?;
            let dir = platform::open_child(&parent.dir, &name)
                .map_err(|e| self.refuse(&path, e.to_string()))?;
            let id = platform::identity(&dir).map_err(|e| self.refuse(&path, e.to_string()))?;
            self.chain.push(Link { path, dir, id });
            self.created += 1;
            let link = self.chain.last().expect("just pushed");
            platform::check(&link.dir, true).map_err(|why| self.refuse(&link.path, why))?;
            platform::is_own(&link.dir).map_err(|why| self.refuse(&link.path, why))?;
        }
        Ok(())
    }

    /// Remove the (empty) folders [`Protected::create_missing`] made,
    /// deepest first, each only while its name in the open parent still
    /// holds it. `rmdir` never removes contents: a folder somebody put
    /// something into stays. One line per folder that stayed.
    pub fn remove_created(&mut self) -> Vec<String> {
        let mut left = Vec::new();
        while self.created > 0 {
            self.created -= 1;
            let Some(link) = self.chain.pop() else {
                break;
            };
            let Some(parent) = self.chain.last() else {
                break;
            };
            let name = link.path.file_name().unwrap_or_default().to_os_string();
            if let Err(e) = platform::rmdir_if(&parent.dir, &name, link.id) {
                left.push(format!(
                    "{} (created by this run) was left in place: {e}",
                    link.path.display()
                ));
                // What is above it is not empty either.
                self.created = 0;
                break;
            }
            self.missing.insert(0, name);
        }
        left
    }
}

/// Admit `path` — a folder that may not exist yet. The nearest existing
/// folder and everything above it must pass; the rest is created later by
/// [`Protected::create_missing`].
pub fn admit(path: &Path) -> Result<Protected, Refusal> {
    let refuse = |component: &Path, reason: String| Refusal {
        path: path.to_path_buf(),
        component: component.to_path_buf(),
        reason,
    };
    if !path.is_absolute() {
        return Err(refuse(
            path,
            pc_core::tr!("нужен полный путь", "a full path is needed").into(),
        ));
    }
    platform::supported().map_err(|why| refuse(path, why))?;
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match (existing.parent(), existing.components().next_back()) {
                    (Some(parent), Some(Component::Normal(name))) => {
                        missing.insert(0, name.to_os_string());
                        existing = parent;
                    }
                    _ => {
                        return Err(refuse(
                            existing,
                            pc_core::tr!(
                                "путь нельзя разобрать на папки",
                                "the path cannot be taken apart into folders"
                            )
                            .into(),
                        ))
                    }
                }
            }
            Err(e) => return Err(refuse(existing, e.to_string())),
        }
    }
    let canonical = fs::canonicalize(existing).map_err(|e| {
        refuse(
            existing,
            pc_core::tf!(
                "не разрешить путь: {0}",
                "the path cannot be resolved: {0}",
                e
            ),
        )
    })?;
    let mut chain: Vec<Link> = Vec::new();
    let mut at = PathBuf::new();
    for component in canonical.components() {
        let dir = match component {
            Component::RootDir => {
                at.push("/");
                platform::open_root()
            }
            Component::Normal(name) => {
                at.push(name);
                let parent = &chain.last().expect("the root comes first").dir;
                platform::open_child(parent, name)
            }
            // `canonicalize` leaves none of these.
            _ => {
                return Err(refuse(
                    &canonical,
                    pc_core::tr!(
                        "путь нельзя разобрать на папки",
                        "the path cannot be taken apart into folders"
                    )
                    .into(),
                ))
            }
        }
        .map_err(|e| {
            refuse(
                &at,
                pc_core::tf!(
                    "не открывается как папка без перехода по ссылкам: {0}",
                    "cannot be opened as a folder without following links: {0}",
                    e
                ),
            )
        })?;
        let id = platform::identity(&dir).map_err(|e| refuse(&at, e.to_string()))?;
        chain.push(Link {
            path: at.clone(),
            dir,
            id,
        });
    }
    let last = chain.len() - 1;
    for (i, link) in chain.iter().enumerate() {
        platform::check(&link.dir, i == last).map_err(|why| refuse(&link.path, why))?;
    }
    let mut full = canonical.clone();
    for name in &missing {
        full.push(name);
    }
    Ok(Protected {
        requested: path.to_path_buf(),
        path: full,
        chain,
        missing,
        created: 0,
    })
}

/// [`admit`] for a folder that must exist already.
pub fn admit_existing(path: &Path) -> Result<Protected, Refusal> {
    let admitted = admit(path)?;
    if admitted.missing.is_empty() {
        Ok(admitted)
    } else {
        Err(Refusal {
            path: path.to_path_buf(),
            component: path.to_path_buf(),
            reason: pc_core::tr!("папки нет", "the folder does not exist").into(),
        })
    }
}

/// A stable identity of the volume holding the object open as `file`: the
/// volume UUID on macOS, the file system ID on Linux. `Err` where there is
/// none that survives a remount — then nothing may be bound to it.
pub fn volume_id(file: &fs::File) -> io::Result<String> {
    platform::volume_id(file)
}

/// A folder this program keeps its data in — a bound thumbnail cache — is
/// its own and nobody else can change it: it belongs to this user, and
/// neither its mode, its access list nor its volume lets another account
/// add, rename, remove or re-permission anything in it. Being inside a
/// protected folder is not enough: a `chmod 777` on the cache itself lets
/// every account empty or fill it.
pub fn check_own_folder(dir: &fs::File) -> Result<(), String> {
    platform::check(dir, true)?;
    platform::is_own(dir)
}

/// How closely [`check_file_at`] looks at a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUse {
    /// Its contents matter (the database and SQLite's `-wal`, `-shm`,
    /// `-journal`): nobody else may write, re-permission or delete it.
    Contents,
    /// Only written to as a note (the writer lock): it must merely be this
    /// user's own plain file with this one name.
    Note,
}

/// The entry `name` in the open folder `parent`, which this program is
/// about to open for writing. `Ok(None)`: there is none, and whatever is
/// created there will be new. `Ok(Some(id))`: a plain file with this one
/// name, belonging to this user, and — for [`FileUse::Contents`] — not
/// writable or re-permissionable by anybody else. Anything else is refused
/// with the reason, before a byte of it is written: a symbolic link, a
/// second name of another file (a hard link) or a special file would pass
/// the write on to whatever it stands for.
pub fn check_file_at(
    parent: &fs::File,
    name: &str,
    what: FileUse,
) -> Result<Option<(u64, u64)>, String> {
    platform::check_file_at(parent, name, what)
}

#[cfg(unix)]
mod platform {
    use std::ffi::{CString, OsStr};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::{fs, io};

    pub(super) fn supported() -> Result<(), String> {
        Ok(())
    }

    pub(super) fn open_root() -> io::Result<fs::File> {
        open_flags(libc::AT_FDCWD, c"/")
    }

    pub(super) fn open_child(parent: &fs::File, name: &OsStr) -> io::Result<fs::File> {
        let name = CString::new(name.as_bytes())?;
        open_flags(parent.as_raw_fd(), &name)
    }

    fn open_flags(dir: libc::c_int, name: &std::ffi::CStr) -> io::Result<fs::File> {
        // SAFETY: a valid descriptor (or AT_FDCWD) and a NUL-terminated
        // name; a returned descriptor is new and owned by the `File`.
        let fd = unsafe {
            libc::openat(
                dir,
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is fresh and nobody else owns it.
        Ok(unsafe { fs::File::from_raw_fd(fd) })
    }

    fn stat(file: &fs::File) -> io::Result<libc::stat> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `st` is writable; initialized on success.
        if unsafe { libc::fstat(file.as_raw_fd(), st.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstat` succeeded.
        Ok(unsafe { st.assume_init() })
    }

    #[allow(clippy::unnecessary_cast)] // the field types differ between systems
    pub(super) fn identity(file: &fs::File) -> io::Result<(u64, u64)> {
        let st = stat(file)?;
        Ok((st.st_dev as u64, st.st_ino as u64))
    }

    /// Why the folder open as `dir` may be changed by somebody other than
    /// this user or root. `last`: it is the folder itself (or the one new
    /// folders are made in), where not even a sticky bit is enough.
    pub(super) fn check(dir: &fs::File, last: bool) -> Result<(), String> {
        let st = stat(dir).map_err(|e| e.to_string())?;
        if st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(pc_core::tr!("не папка", "is not a folder").into());
        }
        // SAFETY: no preconditions.
        let euid = unsafe { libc::geteuid() };
        if st.st_uid != 0 && st.st_uid != euid {
            return Err(pc_core::tf!(
                "принадлежит пользователю {0}, который может её переименовать или изменить \
                 её права",
                "belongs to user {0}, who can rename it or change its permissions",
                st.st_uid
            ));
        }
        let mode = st.st_mode & 0o7777;
        #[allow(clippy::unnecessary_cast)] // `mode_t` differs between systems
        let sticky = mode as u32 & libc::S_ISVTX as u32 != 0;
        if mode & 0o022 != 0 && (last || !sticky) {
            return Err(pc_core::tf!(
                "права {0:o} позволяют другим пользователям добавлять, переименовывать и \
                 удалять в ней записи",
                "permissions {0:o} let other users add, rename and remove entries in it",
                mode
            ));
        }
        let volume = crate::relocate::volume::probe_file(dir)
            .map_err(|e| format!("the properties of its volume cannot be read: {e}"))?;
        crate::relocate::volume::verdict(&volume)?;
        acl::check(dir)
    }

    /// The folder open as `dir` was made by this user just now: it belongs
    /// to this user (not to root, which [`check`] also trusts) and is empty.
    pub(super) fn is_own(dir: &fs::File) -> Result<(), String> {
        let st = stat(dir).map_err(|e| e.to_string())?;
        // SAFETY: no preconditions.
        if st.st_uid != unsafe { libc::geteuid() } {
            return Err(format!(
                "it belongs to user {}, not to this user",
                st.st_uid
            ));
        }
        Ok(())
    }

    pub(super) fn mkdir(parent: &fs::File, name: &OsStr) -> io::Result<()> {
        let name = CString::new(name.as_bytes())?;
        // SAFETY: an open descriptor and a NUL-terminated name.
        if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)]
    pub(super) fn rmdir_if(parent: &fs::File, name: &OsStr, id: (u64, u64)) -> io::Result<()> {
        let name = CString::new(name.as_bytes())?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: as in `stat`.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstatat` succeeded.
        let st = unsafe { st.assume_init() };
        if (st.st_dev as u64, st.st_ino as u64) != id {
            return Err(io::Error::other(
                "another entry is at its name now; it was not touched",
            ));
        }
        // SAFETY: an open descriptor and a NUL-terminated name.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[allow(clippy::unnecessary_cast)] // the field types differ between systems
    pub(super) fn check_file_at(
        parent: &fs::File,
        name: &str,
        what: super::FileUse,
    ) -> Result<Option<(u64, u64)>, String> {
        let cname = CString::new(name).map_err(|e| e.to_string())?;
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: an open descriptor, a NUL-terminated name, a writable
        // buffer initialized on success.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                cname.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(e.to_string())
            };
        }
        // SAFETY: `fstatat` succeeded.
        let st = unsafe { st.assume_init() };
        match st.st_mode & libc::S_IFMT {
            libc::S_IFREG => {}
            libc::S_IFLNK => {
                return Err(pc_core::tr!(
                    "это символическая ссылка: запись попала бы в файл, на который она                      указывает",
                    "it is a symbolic link: a write would land in the file it points to"
                )
                .into())
            }
            _ => {
                return Err(pc_core::tr!(
                    "это не обычный файл",
                    "it is not a plain file"
                )
                .into())
            }
        }
        if st.st_nlink != 1 {
            return Err(pc_core::tf!(
                "у файла {0} имени (жёсткие ссылки): запись изменила бы и файл под другим                  именем",
                "the file has {0} names (hard links): a write would change the file under                  the other name too",
                st.st_nlink
            ));
        }
        // SAFETY: no preconditions.
        let euid = unsafe { libc::geteuid() };
        if st.st_uid != euid {
            return Err(pc_core::tf!(
                "файл принадлежит пользователю {0}, а не этому",
                "the file belongs to user {0}, not to this user",
                st.st_uid
            ));
        }
        let id = (st.st_dev as u64, st.st_ino as u64);
        if what == super::FileUse::Note {
            return Ok(Some(id));
        }
        let mode = st.st_mode & 0o7777;
        if mode & 0o022 != 0 {
            return Err(pc_core::tf!(
                "права {0:o} позволяют другим пользователям менять файл",
                "permissions {0:o} let other users change the file",
                mode
            ));
        }
        // The access list is read through a descriptor; it must be the file
        // just looked at. Read-only and non-blocking: opening writes nothing.
        // SAFETY: as above; a returned descriptor is new and owned below.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                cname.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        // SAFETY: `fd` is fresh and nobody else owns it.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        if identity(&file).map_err(|e| e.to_string())? != id {
            return Err(pc_core::tr!(
                "файл сменился во время проверки",
                "the file changed while it was checked"
            )
            .into());
        }
        acl::check_file(&file)?;
        Ok(Some(id))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn volume_id(file: &fs::File) -> io::Result<String> {
        #[repr(C, packed(4))]
        struct Reply {
            len: u32,
            uuid: [u8; 16],
        }
        // SAFETY: zeroed POD structs; `fgetattrlist` writes at most
        // `size_of::<Reply>()` bytes into `reply`.
        let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
        list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        list.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
        let mut reply: Reply = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::fgetattrlist(
                file.as_raw_fd(),
                (&raw mut list).cast(),
                (&raw mut reply).cast(),
                std::mem::size_of::<Reply>(),
                0,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        let uuid = reply.uuid;
        if uuid == [0; 16] {
            return Err(io::Error::other("the volume has no UUID"));
        }
        Ok(uuid.iter().map(|b| format!("{b:02x}")).collect())
    }

    #[cfg(target_os = "linux")]
    pub(super) fn volume_id(file: &fs::File) -> io::Result<String> {
        let mut st = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: `st` is writable; initialized on success.
        if unsafe { libc::fstatfs(file.as_raw_fd(), st.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fstatfs` succeeded. `f_fsid` is an opaque pair of ints.
        let st = unsafe { st.assume_init() };
        let raw: [i32; 2] = unsafe { std::mem::transmute(st.f_fsid) };
        if raw == [0, 0] {
            return Err(io::Error::other("the file system reports no ID"));
        }
        Ok(format!("{:08x}{:08x}", raw[0] as u32, raw[1] as u32))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    pub(super) fn volume_id(_: &fs::File) -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no stable volume identity on this platform",
        ))
    }

    /// Access lists that could let somebody else change the namespace.
    #[cfg(target_os = "macos")]
    mod acl {
        use std::ffi::{c_int, c_void};
        use std::fs;
        use std::io;
        use std::os::fd::AsRawFd;

        // <sys/acl.h>, <sys/kauth.h>
        const ACL_TYPE_EXTENDED: c_int = 0x100;
        const ACL_FIRST_ENTRY: c_int = 0;
        const ACL_NEXT_ENTRY: c_int = -1;
        const ACL_EXTENDED_ALLOW: c_int = 1;
        /// add_file, delete, add_subdirectory, delete_child,
        /// write_attributes, writesecurity, chown.
        const DANGEROUS: [(c_int, &str); 7] = [
            (1 << 2, "add_file"),
            (1 << 4, "delete"),
            (1 << 5, "add_subdirectory"),
            (1 << 6, "delete_child"),
            (1 << 8, "writeattr"),
            (1 << 12, "writesecurity"),
            (1 << 13, "chown"),
        ];

        extern "C" {
            fn acl_get_fd_np(fd: c_int, kind: c_int) -> *mut c_void;
            fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
            fn acl_get_tag_type(entry: *mut c_void, tag: *mut c_int) -> c_int;
            fn acl_get_permset(entry: *mut c_void, permset: *mut *mut c_void) -> c_int;
            fn acl_get_perm_np(permset: *mut c_void, perm: c_int) -> c_int;
            fn acl_free(obj: *mut c_void) -> c_int;
        }

        /// Refuse an `allow` entry granting anyone any right that changes
        /// entries, the folder itself, or its permissions. `deny` entries
        /// never let anybody in (`everyone deny delete` on home folders).
        /// Inheritance flags do not matter: an inherited right would be
        /// given to the folders created below.
        pub(super) fn check(dir: &fs::File) -> Result<(), String> {
            let found = allowed(dir, &DANGEROUS)?;
            if found.is_empty() {
                return Ok(());
            }
            Err(pc_core::tf!(
                "её список доступа (ACL) разрешает {0}: так другие пользователи могут \
                 менять её содержимое или права",
                "its access list (ACL) allows {0}: other users could change its entries or \
                 permissions",
                found.join(", ")
            ))
        }

        /// For a file: write_data, delete, append_data, writeattr,
        /// writeextattr (the generation lives there), writesecurity, chown.
        const FILE_DANGEROUS: [(c_int, &str); 7] = [
            (1 << 2, "write_data"),
            (1 << 4, "delete"),
            (1 << 5, "append_data"),
            (1 << 8, "writeattr"),
            (1 << 10, "writeextattr"),
            (1 << 12, "writesecurity"),
            (1 << 13, "chown"),
        ];

        pub(super) fn check_file(file: &fs::File) -> Result<(), String> {
            let found = allowed(file, &FILE_DANGEROUS)?;
            if found.is_empty() {
                return Ok(());
            }
            Err(pc_core::tf!(
                "список доступа файла (ACL) разрешает {0}: так другие пользователи могут \
                 менять его",
                "the file's access list (ACL) allows {0}: other users could change it",
                found.join(", ")
            ))
        }

        /// Which of `rights` an `allow` entry grants anybody.
        fn allowed(dir: &fs::File, rights: &[(c_int, &str)]) -> Result<Vec<String>, String> {
            // SAFETY: a valid descriptor; the ACL is freed once below.
            let acl = unsafe { acl_get_fd_np(dir.as_raw_fd(), ACL_TYPE_EXTENDED) };
            if acl.is_null() {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::ENOENT | libc::ENOTSUP | libc::EOPNOTSUPP) => Ok(Vec::new()),
                    _ => Err(format!("its access list cannot be read: {e}")),
                };
            }
            let mut found = Vec::new();
            let mut which = ACL_FIRST_ENTRY;
            loop {
                let mut entry: *mut c_void = std::ptr::null_mut();
                // SAFETY: `acl` is valid until `acl_free`.
                if unsafe { acl_get_entry(acl, which, &mut entry) } != 0 {
                    break;
                }
                which = ACL_NEXT_ENTRY;
                let mut tag: c_int = 0;
                let mut perms: *mut c_void = std::ptr::null_mut();
                // SAFETY: `entry` comes from `acl_get_entry` on a live ACL.
                let readable = unsafe {
                    acl_get_tag_type(entry, &mut tag) == 0
                        && acl_get_permset(entry, &mut perms) == 0
                };
                if !readable {
                    found.push("an entry that cannot be read".to_string());
                    continue;
                }
                if tag != ACL_EXTENDED_ALLOW {
                    continue;
                }
                for &(bit, name) in rights {
                    // SAFETY: `perms` is the entry's permission set.
                    if unsafe { acl_get_perm_np(perms, bit) } == 1 {
                        found.push(name.to_string());
                    }
                }
            }
            // SAFETY: freed once.
            unsafe { acl_free(acl) };
            found.sort();
            found.dedup();
            Ok(found)
        }
    }

    #[cfg(target_os = "linux")]
    mod acl {
        use std::fs;
        use std::io;
        use std::os::fd::AsRawFd;

        /// Any POSIX ACL — access or default — is refused: its named
        /// entries can let other users in whatever the mode bits say.
        pub(super) fn check(dir: &fs::File) -> Result<(), String> {
            for name in [c"system.posix_acl_access", c"system.posix_acl_default"] {
                // SAFETY: a size query with no buffer on a valid descriptor.
                let n = unsafe {
                    libc::fgetxattr(dir.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0)
                };
                if n >= 0 {
                    return Err(pc_core::tr!(
                        "у неё есть список доступа POSIX ACL, который может пускать других \
                         пользователей",
                        "it has a POSIX access list that may let other users in"
                    )
                    .into());
                }
                let e = io::Error::last_os_error();
                // ENOTSUP and EOPNOTSUPP are one value on Linux.
                if !matches!(e.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) {
                    return Err(format!("its access list cannot be read: {e}"));
                }
            }
            Ok(())
        }

        pub(super) fn check_file(file: &fs::File) -> Result<(), String> {
            check(file)
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    mod acl {
        pub(super) fn check(_: &std::fs::File) -> Result<(), String> {
            Err("access lists cannot be checked on this platform".into())
        }

        pub(super) fn check_file(_: &std::fs::File) -> Result<(), String> {
            Err("access lists cannot be checked on this platform".into())
        }
    }
}

#[cfg(not(unix))]
mod platform {
    use std::ffi::OsStr;
    use std::{fs, io};

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            pc_core::tr!(
                "на этой системе программа пока не умеет доказать, что путь не могут \
                 подменить другие пользователи",
                "on this system the program cannot yet prove that other users cannot \
                 redirect the path"
            ),
        )
    }

    pub(super) fn supported() -> Result<(), String> {
        Err(unsupported().to_string())
    }

    pub(super) fn open_root() -> io::Result<fs::File> {
        Err(unsupported())
    }

    pub(super) fn open_child(_: &fs::File, _: &OsStr) -> io::Result<fs::File> {
        Err(unsupported())
    }

    pub(super) fn identity(_: &fs::File) -> io::Result<(u64, u64)> {
        Err(unsupported())
    }

    pub(super) fn check(_: &fs::File, _: bool) -> Result<(), String> {
        Err(unsupported().to_string())
    }

    pub(super) fn is_own(_: &fs::File) -> Result<(), String> {
        Err(unsupported().to_string())
    }

    pub(super) fn mkdir(_: &fs::File, _: &OsStr) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn rmdir_if(_: &fs::File, _: &OsStr, _: (u64, u64)) -> io::Result<()> {
        Err(unsupported())
    }

    pub(super) fn volume_id(_: &fs::File) -> io::Result<String> {
        Err(unsupported())
    }

    pub(super) fn check_file_at(
        _: &fs::File,
        _: &str,
        _: super::FileUse,
    ) -> Result<Option<(u64, u64)>, String> {
        Err(unsupported().to_string())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn set_mode(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        (tmp, root)
    }

    #[test]
    fn a_private_path_is_admitted_as_its_canonical_form() {
        let (_tmp, root) = root();
        let real = root.join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link")).unwrap();
        let admitted = admit(&root.join("link")).unwrap();
        assert_eq!(admitted.path(), real);
        assert!(admitted.dir().is_some());
        admitted.recheck().unwrap();
    }

    /// Others may add their own entries to a sticky folder above ours but
    /// not rename ours; the folder itself (where this app adds entries)
    /// must not be writable by others at all, sticky or not. Group write
    /// counts as others.
    #[test]
    fn write_access_for_others_is_refused_except_a_sticky_ancestor() {
        let (_tmp, root) = root();
        let sticky = root.join("sticky");
        fs::create_dir(&sticky).unwrap();
        set_mode(&sticky, 0o1777);
        let mine = sticky.join("mine");
        fs::create_dir(&mine).unwrap();
        set_mode(&mine, 0o755);
        admit(&mine).unwrap();
        let refused = admit(&sticky).unwrap_err();
        assert_eq!(refused.component, sticky);
        assert!(refused.reason.contains("1777"), "{refused}");
        set_mode(&sticky, 0o777);
        let refused = admit(&mine).unwrap_err();
        assert_eq!(refused.component, sticky, "{refused}");
        set_mode(&sticky, 0o755);
        set_mode(&mine, 0o775);
        let refused = admit(&mine).unwrap_err();
        assert_eq!(refused.component, mine, "{refused}");
        assert_eq!(refused.path, mine);
    }

    /// Missing folders are made one by one, private (0700), checked, and
    /// removed again deepest first; nothing is made before
    /// `create_missing`, and a folder that is not empty stays.
    #[test]
    fn missing_folders_are_made_private_and_removed_again() {
        let (_tmp, root) = root();
        let target = root.join("a/b/c");
        let mut admitted = admit(&target).unwrap();
        assert!(!root.join("a").exists());
        assert!(admitted.dir().is_none());
        admitted.create_missing().unwrap();
        for dir in ["a", "a/b", "a/b/c"] {
            let m = fs::metadata(root.join(dir)).unwrap();
            assert_eq!(m.mode() & 0o777, 0o700, "{dir}");
        }
        admitted.recheck().unwrap();
        fs::write(root.join("a/b/keep"), b"x").unwrap();
        let left = admitted.remove_created();
        assert!(!root.join("a/b/c").exists());
        assert!(root.join("a/b/keep").exists());
        assert_eq!(left.len(), 1, "{left:?}");
        assert!(left[0].contains("a/b"), "{left:?}");
    }

    /// A folder on the path renamed away and replaced after the admission —
    /// even by one with the same name, owner and mode — fails the recheck
    /// with the folder named.
    #[test]
    fn a_replaced_folder_on_the_path_fails_the_recheck() {
        let (_tmp, root) = root();
        let target = root.join("x/data");
        fs::create_dir_all(&target).unwrap();
        let admitted = admit(&target).unwrap();
        fs::rename(root.join("x"), root.join("x-moved")).unwrap();
        fs::create_dir_all(&target).unwrap();
        let refused = admitted.recheck().unwrap_err();
        assert_eq!(refused.component, root.join("x"));
        assert!(refused.reason.contains("replaced"), "{refused}");
    }

    #[test]
    fn relative_paths_and_links_to_nothing_are_refused() {
        let (_tmp, root) = root();
        assert!(admit(Path::new("data")).is_err());
        std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
        let refused = admit(&root.join("dangling/data")).unwrap_err();
        assert_eq!(refused.component, root.join("dangling"));
    }

    /// macOS access lists: `deny` entries and `allow` entries that only
    /// read change nothing; an `allow` of any right that changes entries,
    /// the folder or its permissions — inheritable or not — is refused and
    /// named. The ACL of the folder is never changed by the check.
    #[cfg(target_os = "macos")]
    #[test]
    fn access_lists_that_let_others_change_the_folder_are_refused() {
        use std::process::Command;
        let chmod = |entry: &str, path: &Path| {
            assert!(Command::new("chmod")
                .args(["+a", entry])
                .arg(path)
                .status()
                .unwrap()
                .success())
        };
        let (_tmp, root) = root();
        let ok = root.join("ok");
        fs::create_dir(&ok).unwrap();
        chmod("everyone deny delete", &ok);
        chmod("everyone allow list,search,readattr", &ok);
        admit(&ok).unwrap();
        for (i, rights) in [
            "delete",
            "add_file",
            "add_subdirectory",
            "delete_child",
            "writesecurity",
            "chown",
            "add_file,only_inherit,directory_inherit",
        ]
        .iter()
        .enumerate()
        {
            let dir = root.join(format!("acl{i}"));
            fs::create_dir(&dir).unwrap();
            chmod(&format!("everyone allow {rights}"), &dir);
            let before = Command::new("/bin/ls")
                .arg("-lde")
                .arg(&dir)
                .output()
                .unwrap()
                .stdout;
            let refused = admit(&dir.join("below")).unwrap_err();
            assert_eq!(refused.component, dir, "{rights}");
            let first = rights.split(',').next().unwrap();
            assert!(refused.reason.contains(first), "{rights}: {refused}");
            let after = Command::new("/bin/ls")
                .arg("-lde")
                .arg(&dir)
                .output()
                .unwrap()
                .stdout;
            assert_eq!(before, after);
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn the_volume_has_a_lasting_identity() {
        let (_tmp, root) = root();
        let a = volume_id(&fs::File::open(&root).unwrap()).unwrap();
        let b = volume_id(&fs::File::open("/").unwrap()).unwrap();
        assert!(!a.is_empty() && !b.is_empty());
    }
}
