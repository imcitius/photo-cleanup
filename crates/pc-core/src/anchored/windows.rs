//! The held directory on Windows: handles and `NtCreateFile` relative to a
//! directory handle (el-543ou, design el-61ar8 §2.1).
//!
//! What Windows gives here is not weaker than the Unix descriptors, and in
//! places stronger (observed natively, el-61ar8):
//!
//! - a rename goes through the handle of the object (`rename_into`), not
//!   through its name, so what is renamed is what was checked (P2);
//! - an object held with `DELETE` and shared for reading only cannot be
//!   renamed or opened for writing by anyone else (P3c/d), and while any
//!   handle inside a folder is open, that folder and its ancestors cannot be
//!   renamed (P3g, P3i);
//! - a rename never replaces: an existing name — also one that differs only
//!   in case, a dangling link, or the 8.3 alias of another file — fails the
//!   call (P2a–c, P7d); another volume fails it too and nothing is copied
//!   (P11).
//!
//! Its traps are closed here: a relative open accepts `sub\x` and walks
//! through a junction inside it, and creates `dot.jpg.`, which the Win32 API
//! cannot open again. Every name is therefore checked to be one component
//! that Windows can spell back ([`check_name`]).
//!
//! Nothing is removed: there is no removal method on Windows.

use super::Ident;
use crate::winfs::{self, Kind};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
};

/// Access a held directory asks for: list it, read its attributes, open
/// entries through it. Not `DELETE`: holding a folder never asks to be
/// allowed to move or remove it.
const DIR_ACCESS: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE;

/// Other programs may read, write and rename around a folder held this way;
/// only [`Dir::open_locked`] withholds `FILE_SHARE_DELETE`.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// What identifies the object open as `file` (see [`winfs::ident`]):
/// refused on anything but NTFS.
pub fn ident_of_file(file: &File) -> io::Result<Ident> {
    winfs::ident(file)
}

fn invalid(name: &str, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("not a name Windows can spell back: {name:?} ({why})"),
    )
}

/// One entry name that Windows can open again by path, or a refusal.
///
/// NT itself rejects `..\x`, `x/y` and `a:b` in a rename (el-61ar8 P2d/e,
/// T2a) but accepts `sub\x` in a relative open, walks through a junction in
/// the middle of it (P2g, P6c) and creates `dot.jpg.`, which Win32 cannot
/// see (T2b). A path the journal records must open again through `std::fs`,
/// so such names are neither created nor accepted.
pub fn check_name(name: &str) -> io::Result<&OsStr> {
    if name.is_empty() || name == "." || name == ".." {
        return Err(invalid(name, "not an entry"));
    }
    if let Some(c) = name.chars().find(|&c| {
        (c as u32) < 0x20 || matches!(c, '\\' | '/' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
    }) {
        return Err(invalid(name, &format!("character {c:?}")));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(invalid(name, "ends with a dot or a space"));
    }
    // `CON`, `NUL.jpg`, `com1 .txt`: the device, not a file, whatever the
    // extension. Refusing a few names Windows would allow (`COM0`, the
    // superscript digits on older builds) costs nothing.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let lower = stem.to_lowercase();
    let device = matches!(
        lower.as_str(),
        "con" | "prn" | "aux" | "nul" | "conin$" | "conout$"
    ) || ((lower.starts_with("com") || lower.starts_with("lpt"))
        && lower.chars().count() == 4
        && lower
            .chars()
            .nth(3)
            .is_some_and(|d| d.is_ascii_digit() || matches!(d, '¹' | '²' | '³')));
    if device {
        return Err(invalid(name, "a device name"));
    }
    Ok(OsStr::new(name))
}

/// An entry as itself, read through a handle opened for attributes only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub ident: Ident,
    pub kind: Kind,
    /// `FILE_ATTRIBUTE_*`.
    pub attributes: u32,
    /// `NumberOfLinks`: how many names the object has.
    pub nlink: u64,
    /// `EndOfFile`.
    pub size: u64,
}

impl Entry {
    fn of(file: &File) -> io::Result<Entry> {
        let ident = winfs::ident(file)?;
        let (kind, attributes) = winfs::kind(file)?;
        let standard = winfs::standard(file)?;
        Ok(Entry {
            ident,
            kind,
            attributes,
            nlink: u64::from(standard.NumberOfLinks),
            size: standard.EndOfFile.max(0) as u64,
        })
    }

    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }

    pub fn is_file(&self) -> bool {
        self.kind == Kind::File
    }

    /// A symbolic link or a junction.
    pub fn is_link(&self) -> bool {
        self.kind == Kind::Link
    }
}

/// A folder held open by handle; entries are reached through it.
///
/// There is no removal on Windows (el-543ou, el-61ar8 §2.1): neither of
/// these compiles, so a removal path cannot be assembled here even by
/// mistake —
///
/// ```compile_fail
/// fn remove(d: &pc_core::anchored::Dir) {
///     d.remove_file_at("x.jpg").unwrap();
/// }
/// ```
///
/// ```compile_fail
/// fn remove(d: &pc_core::anchored::Dir) {
///     d.remove_dir_at("x").unwrap();
/// }
/// ```
///
/// — while the rest of the contract does:
///
/// ```
/// fn rename(d: &pc_core::anchored::Dir) {
///     d.rename_no_replace("x.jpg", "y.jpg").unwrap();
/// }
/// ```
pub struct Dir {
    fd: File,
    path: PathBuf,
}

impl Dir {
    /// The folder at `path`, not followed if `path` itself is a link or a
    /// junction (refused, like `O_NOFOLLOW`); ancestors are followed.
    pub fn open(path: &Path) -> io::Result<Dir> {
        Self::open_with(path, FILE_FLAG_OPEN_REPARSE_POINT, SHARE_ALL)
    }

    /// The folder at `path`, following a link or junction at `path` itself:
    /// for a folder the user named, as on Unix.
    pub fn open_following(path: &Path) -> io::Result<Dir> {
        Self::open_with(path, 0, SHARE_ALL)
    }

    /// The folder at `path` (not followed), held so that nobody can rename
    /// or remove it while the handle is open: `FILE_SHARE_DELETE` is
    /// withheld (el-61ar8 P3h; director decision on §5 for the folders of a
    /// unit being moved). Fails with a sharing violation if another program
    /// already has it open for renaming — the folder is busy.
    pub fn open_locked(path: &Path) -> io::Result<Dir> {
        Self::open_with(
            path,
            FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
        )
    }

    fn open_with(path: &Path, flags: u32, share: u32) -> io::Result<Dir> {
        let fd = std::fs::OpenOptions::new()
            .access_mode(DIR_ACCESS)
            .share_mode(share)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | flags)
            .open(path)?;
        let dir = Dir {
            fd,
            path: path.to_path_buf(),
        };
        dir.must_be_a_folder(&path.to_string_lossy())?;
        Ok(dir)
    }

    fn must_be_a_folder(&self, what: &str) -> io::Result<()> {
        match winfs::kind(&self.fd)?.0 {
            Kind::Dir => Ok(()),
            Kind::Link => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{what} is a link or a junction, not a folder"),
            )),
            Kind::File => Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{what} is not a folder"),
            )),
            Kind::Other => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{what} is a folder of a kind this tool does not act on"),
            )),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub fn ident(&self) -> io::Result<Ident> {
        winfs::ident(&self.fd)
    }

    pub fn file(&self) -> &File {
        &self.fd
    }

    fn at(
        &self,
        name: &str,
        access: u32,
        share: u32,
        disposition: u32,
        options: u32,
    ) -> io::Result<File> {
        let name = check_name(name)?;
        winfs::open_at(&self.fd, name, access, share, disposition, options)
    }

    /// The entry `name` as itself (a link or junction is not followed),
    /// opened for its attributes only and shared with everyone, so another
    /// program's handle never makes this fail.
    pub(crate) fn peek(&self, name: &str) -> io::Result<File> {
        self.at(
            name,
            FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
        )
    }

    /// Identity and kind of the entry `name`, as itself.
    pub fn stat_at(&self, name: &str) -> io::Result<(Ident, Kind)> {
        let e = self.entry_at(name)?;
        Ok((e.ident, e.kind))
    }

    pub fn entry_at(&self, name: &str) -> io::Result<Entry> {
        Entry::of(&self.peek(name)?)
    }

    /// The entry `name` as itself, held for a move: `DELETE` (so
    /// [`rename_into`] can move this very object) and read, shared for
    /// reading only. While it is held nobody else can rename it, remove it or
    /// write to it (el-61ar8 P3c/d). If another program already has it open
    /// for writing or renaming — Lightroom, an antivirus scan, a preview —
    /// this fails with a sharing violation: the file is busy, and nothing
    /// was done (P3a).
    ///
    /// A link or a junction is held as itself, never followed (P6).
    pub fn hold(&self, name: &str) -> io::Result<File> {
        self.at(
            name,
            FILE_GENERIC_READ | DELETE,
            FILE_SHARE_READ,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT,
        )
    }

    /// The plain file `name`, not a link and not a folder. For writing it is
    /// shared for reading only, so nobody else writes or renames it
    /// meanwhile.
    pub fn open_file(&self, name: &str, write: bool) -> io::Result<File> {
        let (access, share) = if write {
            (FILE_GENERIC_READ | FILE_GENERIC_WRITE, FILE_SHARE_READ)
        } else {
            (FILE_GENERIC_READ, SHARE_ALL)
        };
        let f = self.at(
            name,
            access,
            share,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )?;
        match winfs::kind(&f)?.0 {
            Kind::File => Ok(f),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} is not a plain file"),
            )),
        }
    }

    /// A new plain file `name`; an existing entry of any kind fails the call
    /// and is not touched (`FILE_CREATE`). Opened with `DELETE` so the
    /// caller can publish this very object by [`rename_into`], and shared
    /// for reading only. `_mode` is the Unix permission: here the folder's
    /// inherited ACL applies, never a wider one.
    pub fn create_new(&self, name: &str, _mode: u32) -> io::Result<File> {
        self.at(
            name,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            FILE_SHARE_READ,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )
    }

    /// A new folder `name`, held; an existing entry fails the call.
    pub fn mkdir(&self, name: &str) -> io::Result<Dir> {
        let fd = self.at(
            name,
            DIR_ACCESS,
            SHARE_ALL,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        )?;
        Ok(Dir {
            fd,
            path: self.path.join(name),
        })
    }

    /// The folder `name` as itself: a link or junction there is refused.
    pub fn open_dir(&self, name: &str) -> io::Result<Dir> {
        self.open_dir_with(name, SHARE_ALL)
    }

    /// The folder `name`, held so nobody can rename it meanwhile (see
    /// [`Dir::open_locked`]).
    pub fn open_dir_locked(&self, name: &str) -> io::Result<Dir> {
        self.open_dir_with(name, FILE_SHARE_READ | FILE_SHARE_WRITE)
    }

    fn open_dir_with(&self, name: &str, share: u32) -> io::Result<Dir> {
        let fd = self.at(
            name,
            DIR_ACCESS,
            share,
            FILE_OPEN,
            FILE_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
        )?;
        let dir = Dir {
            fd,
            path: self.path.join(name),
        };
        dir.must_be_a_folder(name)?;
        Ok(dir)
    }

    /// Rename `from` to `to` in this folder without replacing anything: the
    /// object at `from` is held first and renamed through its handle.
    pub fn rename_no_replace(&self, from: &str, to: &str) -> io::Result<()> {
        let held = self.hold(from)?;
        rename_into(&held, self, to)
    }

    /// The names in this folder, listed through the handle.
    pub fn names(&self) -> io::Result<Vec<OsString>> {
        Ok(winfs::children(&self.fd)?
            .into_iter()
            .map(|(name, _, _)| OsString::from_wide(&name))
            .collect())
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        current_path(&self.fd)
    }

    pub fn try_clone(&self) -> io::Result<Dir> {
        Ok(Dir {
            fd: self.fd.try_clone()?,
            path: self.path.clone(),
        })
    }

    /// Whether `path` leads to this folder now. Compared by identity, so a
    /// path spelled with another case or an 8.3 alias still counts, and a
    /// different folder at the same spelling does not (el-61ar8 P7c). The
    /// last component is followed, as `std::fs::metadata` does on Unix.
    pub fn is_at(&self, path: &Path) -> bool {
        let now = std::fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path);
        match (now.and_then(|f| winfs::ident(&f)), self.ident()) {
            (Ok(now), Ok(held)) => now == held,
            _ => false,
        }
    }
}

/// Rename the object open as `entry` (held with `DELETE`, see [`Dir::hold`])
/// to `name` in the folder `to`, through the handles only: nothing is
/// resolved by path, an existing entry at `name` fails the call and is
/// kept, and another volume fails it with nothing copied.
pub fn rename_into(entry: &File, to: &Dir, name: &str) -> io::Result<()> {
    winfs::rename_into(entry, &to.fd, check_name(name)?)
}

/// What identifies the entry at `path` itself, if anything is there.
pub(super) fn ident_at_path(path: &Path) -> Option<Ident> {
    std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()
        .and_then(|f| winfs::ident(&f).ok())
}

/// Whether the object open as `file` is `obj` and has no name left: its
/// deletion is pending or its last name is gone.
pub(super) fn unlinked(file: &File, obj: Ident) -> bool {
    winfs::ident(file).is_ok_and(|id| id == obj)
        && winfs::standard(file).is_ok_and(|s| s.DeletePending != 0 || s.NumberOfLinks == 0)
}

/// Where the object open as `file` is now, in the spelling a walk records
/// (`C:\…`, `\\server\share\…`), not the system's `\\?\` form.
pub(super) fn current_path(file: &File) -> Option<PathBuf> {
    winfs::final_path(file).map(winfs::without_verbatim)
}
