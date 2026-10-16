//! The object that was checked is the object that moves (el-3wizg, D8 of
//! diagnosis el-5vue3).
//!
//! A path names whatever bears the name *now*. Apply used to compare the
//! pixels of a photograph and its keeper by path and then rename the
//! photograph by path again: anything another program put under that name in
//! between — or under the name of a folder above it — was moved instead,
//! never having been checked.
//!
//! Here every source is reached through its folder, opened once: the folder
//! descriptor is held from the check to the rename, and the rename is
//! `renameat` relative to it, so replacing a folder above cannot redirect the
//! move. Immediately before the rename the entry is asked again through that
//! descriptor and compared with the evidence of the checked object
//! ([`pc_core::proof`]: device, inode, kind, size, mtime, birth time), the
//! keeper likewise; anything else is a refusal that moves nothing. A
//! photograph whose pixels are compared is opened before the comparison and
//! read through that open file, so what was compared is the object the
//! evidence describes.
//!
//! The destination folder is held the same way ([`Destination`]): reached
//! from an anchor through the folder above it, never by a path looked up
//! again, and its recorded path must still lead to it (el-4z6z9 B1).
//!
//! POSIX has no rename conditional on the source's identity: between the
//! last comparison and `renameat` the entry can still be swapped, or the
//! same inode rewritten. So after the rename what arrived is compared once
//! more with the whole evidence — the same [`Proof::check_file`] as before
//! it (el-4z6z9 B2) — through the held destination folder, and *both*
//! recorded folder paths are asked again (el-lvtmk R2). Anything short of
//! the checked object, unchanged, where the journal says it went and from
//! where the journal says it came, is moved back into the folder it came
//! from — never over anything — and the move is refused. Nothing is ever
//! removed (DESIGN §11.2).
//!
//! What is then said about any object is never a path this function was
//! given: it is a [`Whereabouts`], proven after the last rename through the
//! folders and the file held (el-lvtmk R1/R3; `anchored::locate`). A
//! folder moved by another program while the move went on is said as such,
//! and the run that asked stops ([`Unmoved::folder_moved`]). A move that
//! went through hands back what it holds ([`Arrived`]), so its caller asks
//! once more right before the journal records it (el-lvtmk R4), and can put
//! it back with the rest of its unit ([`Arrived::give_back`], `crate::unit`).
//!
//! Scope: interleavings by programs of the same user at the system-call
//! boundary. Another user, root, or a power cut between the rename and the
//! comparison after it are residuals, stated in DESIGN §11.2.
//!
//! Windows (el-6s3go, design el-61ar8 §2.3) takes the same steps through
//! handles, and two of them get stronger. The object is held from its check
//! with `DELETE` and shared for reading only, and the rename goes through
//! *that handle* (`NtSetInformationFile(FileRenameInformation)`, never
//! replacing): what moves is the object checked, not whatever bears its
//! name, and while it is held no other program can write to it or rename it.
//! The folder it is in and the folder it goes to are held without
//! `FILE_SHARE_DELETE`, so neither can be renamed under the move. A file
//! another program holds without sharing deletion (Lightroom, an antivirus,
//! a preview) cannot be held, and is refused as in use before anything
//! moves ([`crate::in_use`]). What another program can still do to a held
//! file is change its times (`FILE_WRITE_ATTRIBUTES` is not governed by
//! sharing): the check right before the rename catches that, but for the
//! window of one call — the residual stated in DESIGN §11.2.

use anyhow::{anyhow, bail, Result};
use pc_core::anchored::{Dir, Ident};
use pc_core::proof::{Kind, Proof, Verdict};
pub use pc_core::whereabouts::Whereabouts;
use std::fs;
use std::path::{Path, PathBuf};

/// What bears `name` in `dir`, as itself: its identity and kind.
fn kind_at(dir: &Dir, name: &str) -> std::io::Result<(Ident, Kind)> {
    #[cfg(unix)]
    {
        let (id, mode) = dir.stat_at(name)?;
        // `S_IFMT` and its values: the same on every Unix this builds for.
        let kind = match mode & 0o170_000 {
            0o120_000 => Kind::Link,
            0o040_000 => Kind::Dir,
            0o100_000 => Kind::File,
            _ => Kind::Other,
        };
        Ok((id, kind))
    }
    #[cfg(windows)]
    {
        dir.stat_at(name)
    }
}

/// The folder an entry is reached through, held from now on. On Windows
/// held without `FILE_SHARE_DELETE` (director decision on el-61ar8 §5.4):
/// no other program renames it while the unit is held, and a folder another
/// program already holds that way is refused as in use.
fn hold_folder(path: &Path) -> std::io::Result<Dir> {
    #[cfg(unix)]
    {
        Dir::open_following(path)
    }
    #[cfg(windows)]
    {
        Dir::open_following_locked(path)
    }
}

/// An entry reached through its folder, opened once.
pub(crate) struct Entry {
    dir: Dir,
    name: String,
    path: PathBuf,
    /// The folder as the path names it.
    parent: PathBuf,
}

impl Entry {
    pub(crate) fn of(path: &Path) -> Result<Entry> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                anyhow!(pc_core::tf!(
                    "не имя файла: {0}",
                    "not a file name: {0}",
                    path.display()
                ))
            })?
            .to_string();
        let parent = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        // An ancestor the user named may be a link (`/var` on macOS); the
        // folder is followed once, here, and held from now on.
        let dir = hold_folder(parent).map_err(|e| {
            if crate::in_use(&e) {
                return anyhow::Error::new(crate::InUse(parent.to_path_buf()));
            }
            anyhow::Error::new(e).context(pc_core::tf!(
                "не открыть папку {0}",
                "cannot open the folder {0}",
                parent.display()
            ))
        })?;
        Ok(Entry {
            dir,
            name,
            path: path.to_path_buf(),
            parent: parent.to_path_buf(),
        })
    }

    /// The object at the name, opened through the held folder and held from
    /// here on (el-lvtmk R7), and whether it is a link. On Unix `None` for a
    /// link, which is never opened (that would follow it) and is checked by
    /// its metadata instead. On Windows every object — a link or junction
    /// as itself — is held with `DELETE`, shared for reading only, and it is
    /// this handle that the move renames.
    #[cfg(unix)]
    fn open_held(&self) -> Result<(Option<fs::File>, bool)> {
        let (_, kind) = kind_at(&self.dir, &self.name).map_err(|e| self.gone(e))?;
        if kind == Kind::Link {
            return Ok((None, true));
        }
        self.dir
            .open_file(&self.name, false)
            .map(|f| (Some(f), false))
            .map_err(|e| self.gone(e))
    }

    /// A cloud placeholder, or any reparse point other than a link or a
    /// junction, is held (opening it as itself recalls nothing) and refused:
    /// moving it inside a synchronised folder would be carried to the cloud
    /// (director decision on el-61ar8 §5.2).
    #[cfg(windows)]
    fn open_held(&self) -> Result<(Option<fs::File>, bool)> {
        let f = self.dir.hold(&self.name).map_err(|e| self.gone(e))?;
        let (kind, _) = pc_core::winfs::kind(&f).map_err(|e| self.gone(e))?;
        if kind == Kind::Other {
            bail!(
                "{}",
                pc_core::tf!(
                    "{0} — объект, который эта программа не переносит (облачная заглушка или иная \
                     точка повторного анализа); ничего не перенесено",
                    "{0} — an object this tool does not move (a cloud placeholder or another \
                     reparse point); nothing moved",
                    self.path.display()
                )
            );
        }
        Ok((Some(f), kind == Kind::Link))
    }

    /// Evidence of what bears the name now, read through the held folder.
    #[cfg(unix)]
    fn proof_now(&self) -> Result<Proof> {
        let md = self.metadata()?;
        Proof::of(&md).ok_or_else(|| {
            anyhow!(pc_core::tr!(
                "эта система не умеет назвать объект файловой системы",
                "this system cannot name a file system object"
            ))
        })
    }

    #[cfg(windows)]
    fn proof_now(&self) -> Result<Proof> {
        kind_at(&self.dir, &self.name).map_err(|e| self.gone(e))?;
        Proof::at(&self.dir, &self.name).ok_or_else(|| {
            anyhow!(pc_core::tr!(
                "эта система не умеет назвать объект файловой системы",
                "this system cannot name a file system object"
            ))
        })
    }

    #[cfg(unix)]
    fn metadata(&self) -> Result<fs::Metadata> {
        let (ident, kind) = kind_at(&self.dir, &self.name).map_err(|e| self.gone(e))?;
        if kind == Kind::Link {
            // A link is never opened (that would follow it); its metadata is
            // read by path, and its identity is compared with what the held
            // folder said a moment ago.
            let md = fs::symlink_metadata(&self.path).map_err(|e| self.gone(e))?;
            if pc_core::anchored::ident_of(&md) != ident {
                bail!("{}", self.differs("another object"));
            }
            return Ok(md);
        }
        let f = self
            .dir
            .open_file(&self.name, false)
            .map_err(|e| self.gone(e))?;
        Ok(f.metadata()?)
    }

    /// The evidence compared with what bears the name now, through the held
    /// folder — by the same question before and after the rename
    /// ([`Proof::check_file`]).
    #[cfg(unix)]
    fn check(&self, expect: &Proof) -> Result<Verdict> {
        let (_, kind) = kind_at(&self.dir, &self.name).map_err(|e| self.gone(e))?;
        if kind == Kind::Link {
            return Ok(expect.check(&self.metadata()?));
        }
        let f = self
            .dir
            .open_file(&self.name, false)
            .map_err(|e| self.gone(e))?;
        Ok(expect.check_file(&f))
    }

    /// On Windows what bears the name is read through the held folder's
    /// handle, as itself ([`Proof::at`]); a recorded content hash cannot be
    /// read that way and is then not a match.
    #[cfg(windows)]
    fn check(&self, expect: &Proof) -> Result<Verdict> {
        kind_at(&self.dir, &self.name).map_err(|e| self.gone(e))?;
        Ok(match Proof::at(&self.dir, &self.name) {
            Some(now) => expect.verify(&now, None),
            None => Verdict::Unprovable("this system cannot name a file system object"),
        })
    }

    fn gone(&self, e: std::io::Error) -> anyhow::Error {
        if crate::in_use(&e) {
            return anyhow::Error::new(crate::InUse(self.path.clone()));
        }
        anyhow!(pc_core::tf!(
            "{0} — не прочитать через удерживаемую папку: {1}; ничего не перенесено",
            "{0} — cannot be read through the held folder: {1}; nothing moved",
            self.path.display(),
            e
        ))
    }

    fn differs(&self, why: &str) -> String {
        pc_core::tf!(
            "{0} — под этим именем уже не проверенный объект ({1}); ничего не перенесено",
            "{0} — the name no longer bears the object that was checked ({1}); nothing moved",
            self.path.display(),
            why
        )
    }

    /// The name still bears the checked object, and the path still leads
    /// to the folder held — so what the journal says is where it was.
    fn verify(&self, expect: &Proof) -> Result<()> {
        if self.dir.is_at(self.dir.path()) {
            return match self.check(expect)? {
                Verdict::Same => Ok(()),
                Verdict::Differs(why) | Verdict::Unprovable(why) => {
                    bail!("{}", self.differs(why))
                }
            };
        }
        bail!(
            "{}",
            pc_core::tf!(
                "{0} — папку {1} заменили после проверки; ничего не перенесено",
                "{0} — the folder {1} was replaced since the check; nothing moved",
                self.path.display(),
                self.dir.path().display()
            )
        )
    }
}

/// A file opened before it is checked and held until it moves: what is read
/// for the check is the object the move is bound to.
pub(crate) struct Held {
    entry: Entry,
    /// The object, open. `None` only on Unix for a link, which is never
    /// opened (that would follow it) and is checked by its metadata
    /// instead. On Windows always the handle the move renames — for a link
    /// or junction, the link itself.
    file: Option<fs::File>,
    /// A link, held (on Windows) or looked at (on Unix) as itself: never
    /// read through.
    link: bool,
    proof: Proof,
}

/// What a photograph is opened for: to be moved, or only to be compared
/// with (a keeper). The difference is on Windows only: a photograph to be
/// moved is held with `DELETE` and shared for reading only; a keeper is
/// opened for reading and shared, so that comparing with it never asks the
/// right to delete it, nor locks another program out of it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    Move,
    Compare,
}

impl Held {
    /// Open the plain file at `path` without following a link at its name.
    /// `Ok(None)` when nothing bears the name.
    pub(crate) fn open(path: &Path, purpose: Purpose) -> Result<Option<Held>> {
        let entry = match Entry::of(path) {
            Ok(e) => e,
            Err(_) if fs::symlink_metadata(path).is_err() => return Ok(None),
            Err(e) => return Err(e),
        };
        let opened = match purpose {
            #[cfg(windows)]
            Purpose::Move => entry.dir.hold(&entry.name),
            _ => entry.dir.open_file(&entry.name, false),
        };
        let link = || kind_at(&entry.dir, &entry.name).is_ok_and(|(_, kind)| kind == Kind::Link);
        let file = match opened {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) if link() => bail!(
                "{}",
                pc_core::tf!(
                    "{0} — символическая ссылка, а не файл",
                    "{0} is a symbolic link, not a file",
                    path.display()
                )
            ),
            Err(e) => return Err(entry.gone(e)),
        };
        let proof = Proof::of_file(&file).ok_or_else(|| {
            anyhow!(pc_core::tr!(
                "эта система не умеет назвать объект файловой системы",
                "this system cannot name a file system object"
            ))
        })?;
        match proof.kind {
            Kind::File => {}
            Kind::Link => bail!(
                "{}",
                pc_core::tf!(
                    "{0} — символическая ссылка, а не файл",
                    "{0} is a symbolic link, not a file",
                    path.display()
                )
            ),
            _ => bail!(
                "{}",
                pc_core::tf!(
                    "{0} — не обычный файл",
                    "{0} is not a regular file",
                    path.display()
                )
            ),
        }
        Ok(Some(Held {
            entry,
            file: Some(file),
            link: false,
            proof,
        }))
    }

    /// Whatever bears the name `path` — a file, a folder of previews — reached
    /// through its folder and held from now until after it is recorded
    /// (el-lvtmk R7), and proven to be the object `expect` describes; without
    /// evidence, the object found now. Every member of a unit is bound this
    /// way before the unit's first rename ([`crate::unit`]).
    pub(crate) fn bind(path: &Path, expect: Option<&Proof>) -> Result<Held> {
        let entry = Entry::of(path)?;
        let (file, link) = entry.open_held()?;
        let proof = match (expect, &file) {
            (Some(p), _) => p.clone(),
            (None, Some(f)) => Proof::of_file(f).ok_or_else(|| {
                anyhow!(pc_core::tr!(
                    "эта система не умеет назвать объект файловой системы",
                    "this system cannot name a file system object"
                ))
            })?,
            (None, None) => entry.proof_now()?,
        };
        let held = Held {
            entry,
            file,
            link,
            proof,
        };
        held.verify()?;
        Ok(held)
    }

    /// The open file, for reading what is checked.
    pub(crate) fn file(&self) -> Result<&fs::File> {
        self.file.as_ref().filter(|_| !self.link).ok_or_else(|| {
            anyhow!(pc_core::tr!(
                "символическая ссылка не открывается",
                "a symbolic link is not opened"
            ))
        })
    }

    /// Where the held object is now, proven, without moving it.
    pub(crate) fn locate(&self) -> Whereabouts {
        pc_core::anchored::locate(
            &self.entry.dir,
            &self.entry.name,
            (self.proof.dev, self.proof.ino),
            Some(&self.entry.path),
            self.file.as_ref(),
        )
    }

    pub(crate) fn proof(&self) -> &Proof {
        &self.proof
    }

    /// The same object, under the same name, unchanged.
    pub(crate) fn verify(&self) -> Result<()> {
        if let Some(f) = &self.file {
            match self.proof.check_file(f) {
                Verdict::Same => {}
                Verdict::Differs(why) | Verdict::Unprovable(why) => {
                    bail!("{}", self.entry.differs(why))
                }
            }
        }
        self.entry.verify(&self.proof)
    }
}

/// Which object a statement about a move is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// The object that was checked, unchanged.
    Checked,
    /// The object that was checked, changed since its check.
    Changed { why: String },
    /// Another object that took its name.
    Stranger,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Checked => "checked",
            Role::Changed { .. } => "changed",
            Role::Stranger => "stranger",
        }
    }
}

/// One object a refused move touched or looked for, and where it is — as
/// far as proven after the last rename (el-lvtmk R1/R3).
#[derive(Debug)]
pub(crate) struct Object {
    pub(crate) role: Role,
    pub(crate) at: Whereabouts,
    pub(crate) side: Side,
    pub(crate) proof: Option<Proof>,
    /// The folder and name it was last proven under, still held, so that
    /// it is located again right before anything is recorded or said about
    /// it (el-4pk1q B2-R3) — never a place sampled earlier.
    pub(crate) kept: Option<Kept>,
}

impl Object {
    /// Where it is now: asked again through what is held, if anything is.
    pub(crate) fn now(&self) -> Whereabouts {
        match &self.kept {
            Some(k) => k.locate(),
            None => self.at.clone(),
        }
    }
}

/// An object's folder and name, held, and the object itself if open.
pub(crate) struct Kept {
    dir: Dir,
    name: String,
    obj: (u64, u64),
    recorded: PathBuf,
    file: Option<fs::File>,
}

impl std::fmt::Debug for Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Kept")
            .field("name", &self.name)
            .field("recorded", &self.recorded)
            .finish_non_exhaustive()
    }
}

impl Kept {
    fn of(
        dir: &Dir,
        name: &str,
        obj: Ident,
        recorded: &Path,
        file: Option<&fs::File>,
    ) -> Option<Kept> {
        Some(Kept {
            dir: dir.try_clone().ok()?,
            name: name.to_string(),
            obj,
            recorded: recorded.to_path_buf(),
            file: file.and_then(|f| f.try_clone().ok()),
        })
    }

    pub(crate) fn locate(&self) -> Whereabouts {
        pc_core::anchored::locate(
            &self.dir,
            &self.name,
            self.obj,
            Some(&self.recorded),
            self.file.as_ref(),
        )
    }
}

/// Which side of one rename an object is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Side {
    /// Back in the folder it was renamed from.
    From,
    /// In the folder it was renamed into.
    To,
    /// Neither, as far as this operation knows: another program put it
    /// wherever it is.
    Elsewhere,
}

/// A bound move that did not happen as asked: in words, and typed.
///
/// `objects` names every object the move touched or looked for, each with
/// its proven [`Whereabouts`]; nothing is said to be anywhere on the
/// strength of the paths the move was given. `folder_moved` is set when a
/// folder of the move stopped leading to the folder held for it: the run
/// that asked stops there (director decision D2 of el-lvtmk).
#[derive(Debug)]
pub(crate) struct Unmoved {
    pub(crate) text: String,
    pub(crate) objects: Vec<Object>,
    pub(crate) folder_moved: bool,
}

impl std::fmt::Display for Unmoved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl std::error::Error for Unmoved {}

/// A move that went through, still holding what it moved: the folder it
/// landed in, the object, and the folder it came from. Asked again right
/// before the move is recorded (el-lvtmk R4), because by then further
/// renames of the same operation, and other programs, have had their turn.
pub(crate) struct Arrived {
    to: Dir,
    name: String,
    from: Dir,
    from_name: String,
    /// The object, still open; on Windows the handle a return renames.
    file: Option<fs::File>,
    dst: PathBuf,
    src: PathBuf,
    proof: Proof,
}

impl std::fmt::Debug for Arrived {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Arrived")
            .field("src", &self.src)
            .field("dst", &self.dst)
            .finish_non_exhaustive()
    }
}

impl Arrived {
    /// Where the moved object is now, proven.
    pub(crate) fn locate(&self) -> Whereabouts {
        pc_core::anchored::locate(
            &self.to,
            &self.name,
            (self.proof.dev, self.proof.ino),
            Some(&self.dst),
            self.file.as_ref(),
        )
    }

    /// The path it came from still leads to the folder it came from.
    pub(crate) fn origin_holds(&self) -> bool {
        self.from.is_at(parent_of(&self.src))
    }

    /// Where the folder it came from is now, as the system names it.
    pub(crate) fn origin_now(&self) -> Option<PathBuf> {
        self.from.current_path()
    }

    pub(crate) fn proof(&self) -> &Proof {
        &self.proof
    }

    /// Put it back where it came from, into the folder held — never over
    /// anything, never by a path looked up again: the compensating return
    /// of a unit that cannot stay where it went ([`crate::unit`]). Only the
    /// object this move carried is put back; if the name it arrived under
    /// bears anything else by now, nothing is renamed.
    pub(crate) fn give_back(&self) -> std::result::Result<(), String> {
        let obj = (self.proof.dev, self.proof.ino);
        match self.to.stat_at(&self.name) {
            Ok((now, _)) if now == obj => {}
            Ok(_) => {
                return Err(pc_core::tr!(
                    "под именем, куда он перенесён, уже другой объект",
                    "another object bears the name it was moved to"
                )
                .to_string())
            }
            Err(e) => return Err(e.to_string()),
        }
        let back = rename_back(
            &self.to,
            &self.name,
            self.file.as_ref(),
            &self.from,
            &self.from_name,
        );
        #[cfg(any(test, feature = "test-seams"))]
        let _ = crate::race::fire_syscall(crate::race::Syscall::AfterReturn, &self.src, &self.dst);
        back.map_err(|e| crate::rename_error(e, &self.dst, &self.src).to_string())
    }

    /// Where it is now, proven: on the side it was last renamed to.
    pub(crate) fn where_now(&self, returned: bool) -> Whereabouts {
        let obj = (self.proof.dev, self.proof.ino);
        if returned {
            pc_core::anchored::locate(
                &self.from,
                &self.from_name,
                obj,
                Some(&self.src),
                self.file.as_ref(),
            )
        } else {
            self.locate()
        }
    }

    pub(crate) fn dst(&self) -> &Path {
        &self.dst
    }
}

fn parent_of(p: &Path) -> &Path {
    match p.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Rename the bound source to `dst` without replacing anything, after
/// checking that the source and every `keepers` entry are still the objects
/// that were checked. See the module comment.
///
/// `Ok` only when what arrived is the checked object, unchanged, under the
/// recorded destination, *and* the recorded source path still leads to the
/// folder it left (el-lvtmk R2). Anything else is an [`Unmoved`]: nothing
/// moved, or what moved was put back — never over anything — and every
/// object is named where it was proven to be after that.
pub(crate) fn rename_bound(source: &Held, dst: &Path, keepers: &[&Held]) -> Result<Arrived> {
    crate::check_exclusive_rename(dst)?;
    {
        let (entry, file, expect): (&Entry, Option<&fs::File>, Proof) =
            (&source.entry, source.file.as_ref(), source.proof.clone());
        let obj = (expect.dev, expect.ino);
        let src = entry.path.as_path();
        // The destination folder is opened — created where missing — once,
        // here, and held to the end; see [`Destination`].
        let to = Destination::open(dst)?;
        // Never onto its own name: Windows accepts renaming an object onto
        // the name it already bears as done, and the journal would record a
        // move that never happened (el-6s3go). Names compare without regard
        // to case — as the folder may.
        if to.dir.ident().ok() == entry.dir.ident().ok()
            && to.name.to_lowercase() == entry.name.to_lowercase()
        {
            bail!(
                "{}",
                pc_core::tf!(
                    "{0} — место назначения и есть сам файл; ничего не перенесено",
                    "{0} — the destination is the file itself; nothing moved",
                    src.display()
                )
            );
        }

        #[cfg(any(test, feature = "test-seams"))]
        crate::race::fire(src, dst).map_err(|e| crate::rename_error(e, src, dst))?;

        // The folder first: if its path leads elsewhere now, the folder was
        // moved — a statement about the whole run, not about this file.
        if !entry.dir.is_at(&entry.parent) {
            let at = pc_core::anchored::locate(&entry.dir, &entry.name, obj, None, file);
            let text = pc_core::tf!(
                "{0} — папку {1} переместили после проверки; ничего не перенесено. Файл: {2}",
                "{0} — the folder {1} was moved since the check; nothing moved. The file: {2}",
                src.display(),
                entry.parent.display(),
                at.shown()
            );
            return Err(Unmoved {
                text,
                objects: vec![Object {
                    role: Role::Checked,
                    at,
                    side: Side::From,
                    proof: Some(expect.clone()),
                    kept: Kept::of(&entry.dir, &entry.name, obj, src, file),
                }],
                folder_moved: true,
            }
            .into());
        }
        match file {
            Some(f) => {
                match expect.check_file(f) {
                    Verdict::Same => {}
                    Verdict::Differs(why) | Verdict::Unprovable(why) => {
                        bail!("{}", entry.differs(why))
                    }
                }
                let held = pc_core::anchored::ident_of_file(f)?;
                match entry.dir.stat_at(&entry.name) {
                    Ok((now, _)) if now == held => {}
                    Ok(_) => bail!("{}", entry.differs("another object")),
                    Err(e) => return Err(entry.gone(e)),
                }
            }
            None => match entry.check(&expect)? {
                Verdict::Same => {}
                Verdict::Differs(why) | Verdict::Unprovable(why) => {
                    bail!("{}", entry.differs(why))
                }
            },
        }
        for k in keepers {
            k.verify().map_err(|e| {
                anyhow!(pc_core::tf!(
                    "сохраняемый файл изменился после проверки: {0}",
                    "the file being kept changed since the check: {0}",
                    e
                ))
            })?;
        }
        if !to.holds() {
            bail!("{}", to.replaced(src));
        }

        #[cfg(any(test, feature = "test-seams"))]
        crate::race::fire_syscall(crate::race::Syscall::Before, src, dst)
            .map_err(|e| crate::rename_error(e, src, dst))?;

        rename_back(&entry.dir, &entry.name, file, &to.dir, &to.name)
            .map_err(|e| crate::rename_error(e, src, dst))?;

        #[cfg(any(test, feature = "test-seams"))]
        let _ = crate::race::fire_syscall(crate::race::Syscall::After, src, dst);

        // The rename cannot ask which object it moved, nor where either
        // folder's path leads by now; all three are asked after it, through
        // the folders held. Success needs every answer (el-lvtmk R2).
        let (verdict, found) = to.arrived(&expect);
        let dst_holds = to.holds();
        let src_holds = entry.dir.is_at(&entry.parent);
        if verdict == Verdict::Same && dst_holds && src_holds {
            return Ok(Arrived {
                to: to.dir.try_clone()?,
                name: to.name.clone(),
                from: entry.dir.try_clone()?,
                from_name: entry.name.clone(),
                file: file.map(fs::File::try_clone).transpose()?,
                dst: dst.to_path_buf(),
                src: src.to_path_buf(),
                proof: expect,
            });
        }
        let moved = found.as_ref().map(|p| (p.dev, p.ino));
        let role = match (&verdict, moved) {
            (_, Some(m)) if m != obj => Role::Stranger,
            (_, None) => Role::Stranger,
            (Verdict::Same, _) => Role::Checked,
            (Verdict::Differs(w) | Verdict::Unprovable(w), _) => Role::Changed {
                why: (*w).to_string(),
            },
        };
        let back = rename_back(&to.dir, &to.name, file, &entry.dir, &entry.name);

        #[cfg(any(test, feature = "test-seams"))]
        let _ = crate::race::fire_syscall(crate::race::Syscall::AfterReturn, src, dst);

        // Where things are is asked now, after the last rename — never
        // taken from a sample before it, nor from the paths given.
        let ours = |m: Ident| if m == obj { file } else { None };
        let at = match (moved, &back) {
            (Some(m), Ok(())) => {
                pc_core::anchored::locate(&entry.dir, &entry.name, m, Some(src), ours(m))
            }
            (Some(m), Err(_)) => {
                pc_core::anchored::locate(&to.dir, &to.name, m, Some(dst), ours(m))
            }
            (None, _) => Whereabouts::uncertain(
                None,
                pc_core::tr!(
                    "под именем, куда он перенесён, ничего не прочитать",
                    "nothing could be read under the name it was moved to"
                ),
            ),
        };
        let kept = match (moved, &back) {
            (Some(m), Ok(())) => Kept::of(&entry.dir, &entry.name, m, src, ours(m)),
            (Some(m), Err(_)) => Kept::of(&to.dir, &to.name, m, dst, ours(m)),
            (None, _) => None,
        };
        let mut objects = vec![Object {
            role: role.clone(),
            at: at.clone(),
            side: if back.is_ok() { Side::From } else { Side::To },
            proof: found.clone(),
            kept,
        }];
        let why = match &verdict {
            Verdict::Differs(w) | Verdict::Unprovable(w) => Some(*w),
            Verdict::Same => None,
        };
        let evidence = pc_core::tf!(
            "проверено: {0}; найдено: {1}",
            "checked: {0}; found: {1}",
            expect.shown(),
            found.as_ref().map_or("-".into(), |p| p.shown())
        );
        let cause = match (&role, why) {
            (Role::Stranger, _) => pc_core::tf!(
                "{0} — в последний момент под этим именем оказался другой объект ({1}); этот объект \
                 перенесён",
                "{0} — another object took this name at the last moment ({1}); that object was moved",
                src.display(),
                evidence
            ),
            (_, Some(w)) => pc_core::tf!(
                "{0} — файл изменился между проверкой и переносом ({1}; {2}); он перенесён",
                "{0} — the file changed between the check and the move ({1}; {2}); it was moved",
                src.display(),
                w,
                evidence
            ),
            (_, None) if !dst_holds && !src_holds => pc_core::tf!(
                "{0} — пути {1} и {2} перестали вести в папки, откуда и куда файл перенесён (их \
                 переместили во время операции); файл перенесён",
                "{0} — the paths {1} and {2} stopped leading to the folders the file was moved \
                 from and into (they were moved during the operation); the file was moved",
                src.display(),
                entry.parent.display(),
                to.parent.display()
            ),
            (_, None) if !dst_holds => pc_core::tf!(
                "{0} — путь {1} перестал вести в папку, куда файл перенесён; файл перенесён",
                "{0} — the path {1} stopped leading to the folder the file was moved into; the \
                 file was moved",
                src.display(),
                to.parent.display()
            ),
            (_, None) => pc_core::tf!(
                "{0} — путь {1} перестал вести в папку, откуда файл перенесён (папку переместили \
                 во время операции); файл перенесён",
                "{0} — the path {1} stopped leading to the folder the file was moved from (the \
                 folder was moved during the operation); the file was moved",
                src.display(),
                entry.parent.display()
            ),
        };
        let outcome = match &back {
            Ok(()) if at.is_verified_at(src) => pc_core::tf!(
                " и возвращён туда, откуда взят: {0}",
                " and returned where it was taken from: {0}",
                at.shown()
            ),
            Ok(()) => pc_core::tf!(
                " и возвращён в свою папку, но не по записанному пути {0}: {1}",
                " and returned into its folder, but not at the recorded path {0}: {1}",
                src.display(),
                at.shown()
            ),
            Err(e) => pc_core::tf!(
                " и не возвращён ({0}); он оставлен, не удалялся: {1}",
                " and could not be returned ({0}); it is kept, not removed: {1}",
                e,
                at.shown()
            ),
        };
        let mut text = format!("{cause}{outcome}");
        if role == Role::Stranger {
            // The checked object is never said to be "not touched" without
            // proof: another program may have taken it from where this
            // operation put it (el-lvtmk #13). Its place is asked through
            // the file held, and said as found.
            let checked = pc_core::anchored::locate(&entry.dir, &entry.name, obj, Some(src), file);
            text.push_str(&pc_core::tf!(
                ". Проверенный файл: {0}",
                ". The checked file: {0}",
                checked.shown()
            ));
            objects.push(Object {
                role: Role::Checked,
                at: checked,
                side: Side::Elsewhere,
                proof: Some(expect.clone()),
                kept: Kept::of(&entry.dir, &entry.name, obj, src, file),
            });
        } else if back.is_ok() {
            text.push_str(pc_core::tr!("; ничего не перенесено", "; nothing moved"));
        }
        Err(Unmoved {
            text,
            objects,
            folder_moved: !dst_holds || !src_holds,
        }
        .into())
    }
}

/// One rename of a bound move, never replacing anything: from the name
/// `from` in the held folder `a` to the name `to` in the held folder `b`.
///
/// On Unix by name (`renameat` with no-replace), relative to the folders
/// held. On Windows the object open as `held` is renamed through its own
/// handle into `b`: what moves is that object whatever bears its name, and
/// `from` only names it for the words.
fn rename_back(
    a: &Dir,
    from: &str,
    held: Option<&fs::File>,
    b: &Dir,
    to: &str,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let _ = held;
        let (from, to) = (cstr(from)?, cstr(to)?);
        pc_core::disk::rename_no_replace_at(a.file(), &from, b.file(), &to)
    }
    #[cfg(windows)]
    {
        let _ = (a, from);
        let held = held.ok_or_else(|| {
            std::io::Error::other(pc_core::tr!(
                "объект не удерживается",
                "the object is not held"
            ))
        })?;
        pc_core::anchored::rename_into(held, b, to)
    }
}

/// The folder a move lands in, held from the moment it is opened (B1 of
/// el-4z6z9).
///
/// A destination looked up by path again at the rename goes wherever its
/// name leads *then*: a folder above it replaced by a link to somewhere
/// else, and the photograph moved there while the journal recorded the
/// intended path — intact, but where neither undo nor the user would look.
///
/// So the folder is reached once, from an anchor, one component at a time
/// through the folder above it ([`Dir::open_dir`]/[`Dir::mkdir`], no link
/// followed), and the rename goes into that descriptor. The anchor is the
/// folder that holds the quarantine's own folder (`.photo-cleanup-quarantine`)
/// when the destination is inside one — that folder and everything below it
/// are this tool's and never a link — and otherwise the nearest folder that
/// exists, which may be reached through the system's or the user's own
/// links (`/var` on macOS): everything below it is created here, as the
/// gathered quarantine's note is (el-usdqi).
///
/// Before the rename and after it, the recorded path is asked where it
/// leads now; if not to the folder held, the move is refused, the file goes
/// back, and if it cannot, the place it is actually kept is named.
struct Destination {
    dir: Dir,
    name: String,
    /// The folder as the journal records it.
    parent: PathBuf,
}

impl Destination {
    fn open(dst: &Path) -> Result<Destination> {
        let name = dst
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                anyhow!(pc_core::tf!(
                    "не имя файла: {0}",
                    "not a file name: {0}",
                    dst.display()
                ))
            })?
            .to_string();
        let parent = match dst.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        };
        let parts: Vec<std::path::Component> = parent.components().collect();
        let ours = parts
            .iter()
            .position(|c| c.as_os_str() == pc_core::QUARANTINE_DIR);
        let limit: PathBuf = match ours {
            Some(0) => PathBuf::from("."),
            Some(i) => parts[..i].iter().collect(),
            None => parent.clone(),
        };
        let mut at = limit.as_path();
        while fs::symlink_metadata(at).is_err() {
            match at.parent() {
                Some(p) if !p.as_os_str().is_empty() => at = p,
                _ => {
                    at = Path::new(".");
                    break;
                }
            }
        }
        let cannot_create = |e: std::io::Error| {
            anyhow::Error::new(e).context(pc_core::tf!(
                "не создать каталог карантина {0} — если файловая система смонтирована \
                 в корень или недоступна на запись, задайте --quarantine <путь на том же диске>",
                "cannot create the quarantine folder {0} — if the file system is mounted \
                 at the root or read-only, pass --quarantine <a path on the same disk>",
                parent.display()
            ))
        };
        let below = if at == Path::new(".") && parent.is_relative() {
            parent.as_path()
        } else {
            parent.strip_prefix(at).unwrap_or(Path::new(""))
        };
        let mut names = Vec::new();
        for part in below.components() {
            let std::path::Component::Normal(n) = part else {
                if part == std::path::Component::CurDir {
                    continue;
                }
                bail!(
                    "{}",
                    pc_core::tf!(
                        "{0} — путь карантина должен быть без «..»",
                        "{0} — a destination path has to be without \"..\"",
                        parent.display()
                    )
                );
            };
            names.push(n.to_str().ok_or_else(|| {
                anyhow!(pc_core::tf!(
                    "не имя папки: {0}",
                    "not a folder name: {0}",
                    parent.display()
                ))
            })?);
        }
        // The folder the move lands in is held like the one it leaves
        // ([`hold_folder`]); the folders above it, only as a way there.
        let in_use = |e: std::io::Error, at: &Path| {
            if crate::in_use(&e) {
                anyhow!("{}", crate::in_use_text(at))
            } else {
                cannot_create(e)
            }
        };
        let mut dir = if names.is_empty() {
            hold_folder(at).map_err(|e| in_use(e, at))?
        } else {
            Dir::open_following(at).map_err(cannot_create)?
        };
        for (i, n) in names.iter().enumerate() {
            let last = i + 1 == names.len();
            dir = match sub(&dir, n, last, false) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    match sub(&dir, n, last, true) {
                        Ok(d) => d,
                        // Someone else made it meanwhile: used, but only as a
                        // real folder — it is opened without following.
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            sub(&dir, n, last, false).map_err(|e| not_ours(&dir, n, e))?
                        }
                        Err(e) if crate::in_use(&e) => {
                            return Err(in_use(e, &dir.join(n)));
                        }
                        Err(e) => return Err(cannot_create(e)),
                    }
                }
                Err(e) if crate::in_use(&e) => return Err(in_use(e, &dir.join(n))),
                Err(e) => return Err(not_ours(&dir, n, e)),
            };
        }
        Ok(Destination { dir, name, parent })
    }

    /// The recorded folder path still leads to the folder held.
    fn holds(&self) -> bool {
        self.dir.is_at(&self.parent)
    }

    /// What bears the name in the folder held, compared with the evidence;
    /// and its own evidence, for the record.
    fn arrived(&self, expect: &Proof) -> (Verdict, Option<Proof>) {
        let Ok((_, kind)) = kind_at(&self.dir, &self.name) else {
            return (Verdict::Unprovable("nothing bears the name"), None);
        };
        #[cfg(unix)]
        {
            if kind == Kind::Link {
                return (Verdict::Differs("another object"), None);
            }
            match self.dir.open_file(&self.name, false) {
                Ok(f) => (expect.check_file(&f), Proof::of_file(&f)),
                Err(_) => (Verdict::Unprovable("it can no longer be opened"), None),
            }
        }
        // Read as itself through the folder's handle: a link or junction
        // is its own object here, and the comparison says whether it is
        // the one checked.
        #[cfg(windows)]
        {
            let _ = kind;
            match Proof::at(&self.dir, &self.name) {
                Some(found) => (expect.verify(&found, None), Some(found)),
                None => (Verdict::Unprovable("it can no longer be opened"), None),
            }
        }
    }

    fn replaced(&self, src: &Path) -> String {
        pc_core::tf!(
            "{0} — путь {1} больше не ведёт в папку, открытую для переноса; ничего не перенесено",
            "{0} — the path {1} no longer leads to the folder opened for the move; nothing moved",
            src.display(),
            self.parent.display()
        )
    }
}

/// One folder below `dir` on the way to a destination: opened, or created
/// (`create`), without following anything. The last one, the folder the
/// move lands in, is held on Windows without `FILE_SHARE_DELETE`
/// ([`hold_folder`]).
fn sub(dir: &Dir, name: &str, last: bool, create: bool) -> std::io::Result<Dir> {
    #[cfg(windows)]
    if last {
        return if create {
            dir.mkdir_locked(name)
        } else {
            dir.open_dir_locked(name)
        };
    }
    let _ = last;
    if create {
        dir.mkdir(name)
    } else {
        dir.open_dir(name)
    }
}

/// A component of the destination that cannot be opened as a real folder
/// without following anything: a link or a file in its place is named as
/// such; any other failure is told as it is.
fn not_ours(dir: &Dir, name: &str, e: std::io::Error) -> anyhow::Error {
    let at = dir.join(name);
    let at = at.as_path();
    let found = kind_at(dir, name);
    let folder = found.as_ref().is_ok_and(|(_, kind)| *kind == Kind::Dir);
    if folder || found.is_err() {
        return anyhow::Error::new(e).context(pc_core::tf!(
            "не открыть папку {0}; ничего не перенесено",
            "cannot open the folder {0}; nothing moved",
            at.display()
        ));
    }
    anyhow!(pc_core::tf!(
        "{0} — не папка, а ссылка или файл ({1}); через неё ничего не переносится, ничего не перенесено",
        "{0} — not a folder but a link or a file ({1}); nothing is moved through it, nothing moved",
        at.display(),
        e
    ))
}

#[cfg(unix)]
fn cstr(name: &str) -> std::io::Result<std::ffi::CString> {
    Ok(std::ffi::CString::new(name)?)
}
