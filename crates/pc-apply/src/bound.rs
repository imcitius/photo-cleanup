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

use anyhow::{anyhow, bail, Result};
use pc_core::proof::{Proof, Verdict};
pub use pc_core::whereabouts::Whereabouts;

#[cfg(unix)]
type Ident = pc_core::anchored::Ident;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use pc_core::anchored::Dir;

/// An entry reached through its folder, opened once.
#[cfg(unix)]
pub(crate) struct Entry {
    dir: Dir,
    name: String,
    path: PathBuf,
    /// The folder as the path names it.
    parent: PathBuf,
}

#[cfg(unix)]
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
        let dir = Dir::open_following(parent).map_err(|e| {
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
    /// here on (el-lvtmk R7) — `None` for a link, which is never opened
    /// (that would follow it) and is checked by its metadata instead.
    fn open_held(&self) -> Result<Option<fs::File>> {
        let (_, mode) = self.dir.stat_at(&self.name).map_err(|e| self.gone(e))?;
        if mode & 0o170_000 == 0o120_000 {
            return Ok(None);
        }
        self.dir
            .open_file(&self.name, false)
            .map(Some)
            .map_err(|e| self.gone(e))
    }

    /// Evidence of what bears the name now, read through the held folder.
    fn proof_now(&self) -> Result<Proof> {
        let md = self.metadata()?;
        Proof::of(&md).ok_or_else(|| {
            anyhow!(pc_core::tr!(
                "эта система не умеет назвать объект файловой системы",
                "this system cannot name a file system object"
            ))
        })
    }

    fn metadata(&self) -> Result<fs::Metadata> {
        let (ident, mode) = self.dir.stat_at(&self.name).map_err(|e| self.gone(e))?;
        // `S_IFMT`/`S_IFLNK`: the same values on every Unix this builds for.
        if mode & 0o170_000 == 0o120_000 {
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
    fn check(&self, expect: &Proof) -> Result<Verdict> {
        let (_, mode) = self.dir.stat_at(&self.name).map_err(|e| self.gone(e))?;
        if mode & 0o170_000 == 0o120_000 {
            return Ok(expect.check(&self.metadata()?));
        }
        let f = self
            .dir
            .open_file(&self.name, false)
            .map_err(|e| self.gone(e))?;
        Ok(expect.check_file(&f))
    }

    fn gone(&self, e: std::io::Error) -> anyhow::Error {
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
        if let Ok(now) = fs::metadata(self.dir.path()) {
            if pc_core::anchored::ident_of(&now) == self.dir.ident()? {
                return match self.check(expect)? {
                    Verdict::Same => Ok(()),
                    Verdict::Differs(why) | Verdict::Unprovable(why) => {
                        bail!("{}", self.differs(why))
                    }
                };
            }
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
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) struct Held {
    #[cfg(unix)]
    entry: Entry,
    /// `None` only for a link, which is never opened (that would follow
    /// it) and is checked by its metadata instead.
    file: Option<fs::File>,
    proof: Proof,
}

impl Held {
    /// Open the plain file at `path` without following a link at its name.
    /// `Ok(None)` when nothing bears the name.
    pub(crate) fn open(path: &Path) -> Result<Option<Held>> {
        #[cfg(unix)]
        {
            let entry = match Entry::of(path) {
                Ok(e) => e,
                Err(_) if fs::symlink_metadata(path).is_err() => return Ok(None),
                Err(e) => return Err(e),
            };
            let file = match entry.dir.open_file(&entry.name, false) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_)
                    if entry
                        .dir
                        .stat_at(&entry.name)
                        .is_ok_and(|(_, mode)| mode & 0o170_000 == 0o120_000) =>
                {
                    bail!(
                        "{}",
                        pc_core::tf!(
                            "{0} — символическая ссылка, а не файл",
                            "{0} is a symbolic link, not a file",
                            path.display()
                        )
                    )
                }
                Err(e) => return Err(entry.gone(e)),
            };
            let md = file.metadata()?;
            if !md.is_file() {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "{0} — не обычный файл",
                        "{0} is not a regular file",
                        path.display()
                    )
                );
            }
            let proof = Proof::of(&md).ok_or_else(|| anyhow!("no evidence"))?;
            Ok(Some(Held {
                entry,
                file: Some(file),
                proof,
            }))
        }
        #[cfg(not(unix))]
        {
            // Nothing moves on this system (`check_exclusive_rename`), and
            // holding a file by its folder is not verified here: refuse.
            let _ = path;
            bail!(
                "{}",
                pc_core::tr!(
                    "на этой системе файл не удерживается от проверки до переноса",
                    "this system cannot hold a file from its check to its move"
                )
            )
        }
    }

    /// Whatever bears the name `path` — a file, a folder of previews — reached
    /// through its folder and held from now until after it is recorded
    /// (el-lvtmk R7), and proven to be the object `expect` describes; without
    /// evidence, the object found now. Every member of a unit is bound this
    /// way before the unit's first rename ([`crate::unit`]).
    pub(crate) fn bind(path: &Path, expect: Option<&Proof>) -> Result<Held> {
        #[cfg(unix)]
        {
            let entry = Entry::of(path)?;
            let file = entry.open_held()?;
            let proof = match (expect, &file) {
                (Some(p), _) => p.clone(),
                (None, Some(f)) => Proof::of(&f.metadata()?).ok_or_else(|| {
                    anyhow!(pc_core::tr!(
                        "эта система не умеет назвать объект файловой системы",
                        "this system cannot name a file system object"
                    ))
                })?,
                (None, None) => entry.proof_now()?,
            };
            let held = Held { entry, file, proof };
            held.verify()?;
            Ok(held)
        }
        #[cfg(not(unix))]
        {
            let _ = (path, expect);
            bail!(
                "{}",
                pc_core::tr!(
                    "на этой системе файл не удерживается от проверки до переноса",
                    "this system cannot hold a file from its check to its move"
                )
            )
        }
    }

    /// The open file, for reading what is checked.
    pub(crate) fn file(&self) -> Result<&fs::File> {
        self.file.as_ref().ok_or_else(|| {
            anyhow!(pc_core::tr!(
                "символическая ссылка не открывается",
                "a symbolic link is not opened"
            ))
        })
    }

    /// Where the held object is now, proven, without moving it.
    pub(crate) fn locate(&self) -> Whereabouts {
        #[cfg(unix)]
        {
            pc_core::anchored::locate(
                &self.entry.dir,
                &self.entry.name,
                (self.proof.dev, self.proof.ino),
                Some(&self.entry.path),
                self.file.as_ref(),
            )
        }
        #[cfg(not(unix))]
        Whereabouts::uncertain(None, "unsupported")
    }

    pub(crate) fn proof(&self) -> &Proof {
        &self.proof
    }

    /// The same object, under the same name, unchanged.
    pub(crate) fn verify(&self) -> Result<()> {
        #[cfg(unix)]
        {
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
        #[cfg(not(unix))]
        bail!("unsupported")
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
        #[cfg(unix)]
        {
            pc_core::anchored::locate(
                &self.dir,
                &self.name,
                self.obj,
                Some(&self.recorded),
                self.file.as_ref(),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (&self.name, self.obj, &self.recorded, &self.file);
            Whereabouts::uncertain(None, "unsupported")
        }
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
    #[cfg(unix)]
    to: Dir,
    #[cfg(unix)]
    name: String,
    #[cfg(unix)]
    from: Dir,
    #[cfg(unix)]
    from_name: String,
    #[cfg(unix)]
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
        #[cfg(unix)]
        {
            pc_core::anchored::locate(
                &self.to,
                &self.name,
                (self.proof.dev, self.proof.ino),
                Some(&self.dst),
                self.file.as_ref(),
            )
        }
        #[cfg(not(unix))]
        Whereabouts::uncertain(None, "unsupported")
    }

    /// The path it came from still leads to the folder it came from.
    pub(crate) fn origin_holds(&self) -> bool {
        #[cfg(unix)]
        {
            self.from.is_at(parent_of(&self.src))
        }
        #[cfg(not(unix))]
        false
    }

    /// Where the folder it came from is now, as the system names it.
    pub(crate) fn origin_now(&self) -> Option<PathBuf> {
        #[cfg(unix)]
        {
            self.from.current_path()
        }
        #[cfg(not(unix))]
        None
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
        #[cfg(unix)]
        {
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
            let (a, b) = (
                cstr(&self.name).map_err(|e| e.to_string())?,
                cstr(&self.from_name).map_err(|e| e.to_string())?,
            );
            let back =
                pc_core::disk::rename_no_replace_at(self.to.file(), &a, self.from.file(), &b);
            #[cfg(any(test, feature = "test-seams"))]
            let _ =
                crate::race::fire_syscall(crate::race::Syscall::AfterReturn, &self.src, &self.dst);
            back.map_err(|e| crate::rename_error(e, &self.dst, &self.src).to_string())
        }
        #[cfg(not(unix))]
        Err("unsupported".into())
    }

    /// Where it is now, proven: on the side it was last renamed to.
    pub(crate) fn where_now(&self, returned: bool) -> Whereabouts {
        #[cfg(unix)]
        {
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
        #[cfg(not(unix))]
        {
            let _ = returned;
            Whereabouts::uncertain(None, "unsupported")
        }
    }

    pub(crate) fn dst(&self) -> &Path {
        &self.dst
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
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
    #[cfg(unix)]
    {
        let (entry, file, expect): (&Entry, Option<&fs::File>, Proof) =
            (&source.entry, source.file.as_ref(), source.proof.clone());
        let obj = (expect.dev, expect.ino);
        let src = entry.path.as_path();
        // The destination folder is opened — created where missing — once,
        // here, and held to the end; see [`Destination`].
        let to = Destination::open(dst)?;

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
                let held = pc_core::anchored::ident_of(&f.metadata()?);
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

        let (a, b) = (cstr(&entry.name)?, cstr(&to.name)?);
        pc_core::disk::rename_no_replace_at(entry.dir.file(), &a, to.dir.file(), &b)
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
        let back = pc_core::disk::rename_no_replace_at(to.dir.file(), &b, entry.dir.file(), &a);

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
    #[cfg(not(unix))]
    {
        let _ = (source, keepers);
        bail!("unsupported")
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
#[cfg(unix)]
struct Destination {
    dir: Dir,
    name: String,
    /// The folder as the journal records it.
    parent: PathBuf,
}

#[cfg(unix)]
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
        let mut dir = Dir::open_following(at).map_err(cannot_create)?;
        let below = if at == Path::new(".") && parent.is_relative() {
            parent.as_path()
        } else {
            parent.strip_prefix(at).unwrap_or(Path::new(""))
        };
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
            let n = n.to_str().ok_or_else(|| {
                anyhow!(pc_core::tf!(
                    "не имя папки: {0}",
                    "not a folder name: {0}",
                    parent.display()
                ))
            })?;
            dir = match dir.open_dir(n) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => match dir.mkdir(n) {
                    Ok(d) => d,
                    // Someone else made it meanwhile: used, but only as a
                    // real folder — it is opened without following.
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        dir.open_dir(n).map_err(|e| not_ours(&dir, n, e))?
                    }
                    Err(e) => return Err(cannot_create(e)),
                },
                Err(e) => return Err(not_ours(&dir, n, e)),
            };
        }
        Ok(Destination { dir, name, parent })
    }

    /// The recorded folder path still leads to the folder held.
    fn holds(&self) -> bool {
        match (fs::metadata(&self.parent), self.dir.ident()) {
            (Ok(now), Ok(held)) => pc_core::anchored::ident_of(&now) == held,
            _ => false,
        }
    }

    /// What bears the name in the folder held, compared with the evidence;
    /// and its own evidence, for the record.
    fn arrived(&self, expect: &Proof) -> (Verdict, Option<Proof>) {
        let Ok((_, mode)) = self.dir.stat_at(&self.name) else {
            return (Verdict::Unprovable("nothing bears the name"), None);
        };
        if mode & 0o170_000 == 0o120_000 {
            return (Verdict::Differs("another object"), None);
        }
        match self.dir.open_file(&self.name, false) {
            Ok(f) => {
                let found = f.metadata().ok().and_then(|m| Proof::of(&m));
                (expect.check_file(&f), found)
            }
            Err(_) => (Verdict::Unprovable("it can no longer be opened"), None),
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

/// A component of the destination that cannot be opened as a real folder
/// without following anything: a link or a file in its place is named as
/// such; any other failure is told as it is.
#[cfg(unix)]
fn not_ours(dir: &Dir, name: &str, e: std::io::Error) -> anyhow::Error {
    let at = dir.join(name);
    let at = at.as_path();
    // `S_IFMT`/`S_IFDIR`, as above.
    let folder = dir
        .stat_at(name)
        .is_ok_and(|(_, mode)| mode & 0o170_000 == 0o040_000);
    if folder || dir.stat_at(name).is_err() {
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
fn cstr(name: &str) -> Result<std::ffi::CString> {
    Ok(std::ffi::CString::new(name)?)
}
