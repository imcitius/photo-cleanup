//! What a gathered quarantine says about itself.
//!
//! Read from `QUARANTINE_LAYOUT` beside the data. Missing or unreadable means
//! "this is not a gathered quarantine, or nothing recorded it" — and then the
//! way home is not guessed.
//!
//! The note is written into a folder the user named, so nothing there is
//! assumed to be ours (el-23goa, el-5vue3 R1/D1/D2):
//!
//! - every entry is reached through the open quarantine folder, never by a
//!   path looked up again ([`crate::anchored`]);
//! - a new note is a fresh file (`O_EXCL|O_NOFOLLOW`), published by a rename
//!   that never replaces, and then compared with the file this call wrote: a
//!   substituted temporary is not accepted as our note;
//! - a temporary that could not be published is never removed (user
//!   decision 2026-10-04, el-1y8uo B1: POSIX has no conditional unlink). It
//!   stays where it is — or the stranger now at its name does — and the
//!   error names it as retained;
//! - folders this call created are never removed again: an empty folder at a
//!   name is not proof it is still the one created. They are reported;
//! - an existing note is extended only if it says what it is (the versioned
//!   format below), is a plain file with one name, belongs to this user, and
//!   is still the object at the name when it is written. A note in the
//!   earlier unversioned format is read, and accepted as it is when it
//!   already records the label, but not rewritten. Anything else is refused,
//!   untouched.
//!
//! Extending in place is not atomic: a crash in the middle of the write can
//! leave a note that no longer parses. The next run then refuses it — the
//! data is not touched — and the reason names the file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where each disk label the gathered quarantine uses was mounted.
pub type Disks = BTreeMap<String, String>;

/// What the note says it is.
pub const FORMAT: &str = "photo-cleanup quarantine layout";
pub const VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct Versioned {
    format: String,
    version: u32,
    disks: Disks,
}

/// How an existing note reads.
enum Parsed {
    Versioned(Disks),
    /// Written by an earlier version: a bare map of label to mount.
    Unversioned(Disks),
}

fn parse(raw: &str) -> Option<Parsed> {
    if let Ok(v) = serde_json::from_str::<Versioned>(raw) {
        return (v.format == FORMAT && v.version == VERSION).then_some(Parsed::Versioned(v.disks));
    }
    let disks: Disks = serde_json::from_str(raw).ok()?;
    let plausible = disks.iter().all(|(label, mount)| {
        !label.is_empty()
            && !label.contains(['/', '\\'])
            && label != "."
            && label != ".."
            && Path::new(mount).is_absolute()
    });
    (plausible && !disks.is_empty()).then_some(Parsed::Unversioned(disks))
}

fn file(root: &Path) -> PathBuf {
    root.join(crate::QUARANTINE_LAYOUT)
}

pub fn read(root: &Path) -> Disks {
    match std::fs::read_to_string(file(root))
        .ok()
        .and_then(|raw| parse(&raw))
    {
        Some(Parsed::Versioned(d) | Parsed::Unversioned(d)) => d,
        None => Disks::default(),
    }
}

/// Something this call left in place rather than remove, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retained {
    pub path: PathBuf,
    pub why: String,
}

/// The note could not be written. `cause` is what refused; `retained` is
/// every entry this call created or met and deliberately did not remove.
#[derive(Debug)]
pub struct Refusal {
    pub cause: std::io::Error,
    pub retained: Vec<Retained>,
}

impl From<std::io::Error> for Refusal {
    fn from(cause: std::io::Error) -> Self {
        Refusal {
            cause,
            retained: Vec::new(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.cause)?;
        if !self.retained.is_empty() {
            let list = self
                .retained
                .iter()
                .map(|r| format!("{} — {}", r.path.display(), r.why))
                .collect::<Vec<_>>()
                .join("; ");
            write!(
                f,
                ". {}",
                crate::tf!(
                    "Оставлено на месте, не удалялось: {0}",
                    "Left in place, not removed: {0}",
                    list
                )
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for Refusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}

fn foreign(path: &Path, (ru, en): (&str, &str)) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        crate::tf!(
            "{0} — {1}: чужое не перезаписывается, уберите или переименуйте его",
            "{0} — {1}: something not ours is not written over; move or rename it",
            path.display(),
            crate::tr!(ru, en)
        ),
    )
}

/// Record that this label stood for this mount point.
///
/// Written before the first file of a disk lands and left alone afterwards,
/// so the note is there for anything that arrives later — and so a reader
/// finds it whatever order the moves happened in. Call this only once the
/// move it serves has been admitted: it writes.
pub fn note(root: &Path, label: &str, mount: &Path) -> Result<(), Refusal> {
    #[cfg(unix)]
    {
        unix::note(root, label, &mount.display().to_string())
    }
    #[cfg(not(unix))]
    {
        let _ = (label, mount);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            crate::tf!(
                "раскладка карантина {0} не пишется: на Windows создание и проверка файлов через удерживаемый каталог ещё не проверены на настоящей системе",
                "the quarantine layout in {0} is not written: on Windows, creating and checking files through a held folder has not been verified on a real system yet",
                root.display()
            ),
        )
        .into())
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    #[cfg(test)]
    use crate::anchored::seam::{self, Stage};
    use crate::anchored::{ident_of, Dir, Ident};
    use std::io::{self, Read, Seek, Write};

    /// Bigger than any note this tool writes by orders of magnitude.
    const MAX_NOTE: u64 = 1 << 20;

    pub(super) fn note(root: &Path, label: &str, mount: &str) -> Result<(), Refusal> {
        let mut retained = Vec::new();
        let done = open_or_create(root, &mut retained).and_then(|dir| {
            publish(&dir, label, mount, &mut retained)?;
            Ok(())
        });
        done.map_err(|cause| Refusal { cause, retained })
    }

    /// The quarantine folder, opened; created (and reported if the note then
    /// fails) when it is not there.
    fn open_or_create(root: &Path, retained: &mut Vec<Retained>) -> io::Result<Dir> {
        match Dir::open(root) {
            Ok(d) => return Ok(d),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(foreign(root, ("символическая ссылка", "a symbolic link")))
            }
            Err(e) if e.raw_os_error() == Some(libc::ENOTDIR) => {
                if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
                    return Err(foreign(root, ("символическая ссылка", "a symbolic link")));
                }
                return Err(foreign(root, ("не каталог", "not a folder")));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Up to the nearest folder that exists; that one may be reached
        // through the system's own links (`/var` on macOS). Everything below
        // it is created here, one component at a time, through the folder
        // above it.
        let mut missing = Vec::new();
        let mut at = root;
        while std::fs::symlink_metadata(at).is_err() {
            let name = at
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unnamed path"))?;
            missing.push(name.to_string());
            match at.parent() {
                Some(p) if !p.as_os_str().is_empty() => at = p,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "no existing ancestor",
                    ))
                }
            }
        }
        let mut dir = open_existing_ancestor(at)?;
        for name in missing.iter().rev() {
            dir = match dir.mkdir(name) {
                Ok(d) => {
                    retained.push(Retained {
                        path: d.path().to_path_buf(),
                        why: crate::tr!(
                            "каталог создан этой операцией; пустой каталог не удаляется: ничто не доказывает в момент удаления, что под этим именем всё ещё он",
                            "folder created by this operation; an empty folder is not removed: nothing proves at removal time that the name is still it"
                        )
                        .into(),
                    });
                    d
                }
                // Someone else made it meanwhile: used, never claimed.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => dir.open_dir(name)?,
                Err(e) => return Err(e),
            };
        }
        Ok(dir)
    }

    /// The nearest existing ancestor may be reached through the system's
    /// own links (`/var` on macOS): it is followed on purpose. Everything
    /// below it is created and opened without following.
    fn open_existing_ancestor(at: &Path) -> io::Result<Dir> {
        Dir::open_following(at)
    }

    fn publish(
        dir: &Dir,
        label: &str,
        mount: &str,
        retained: &mut Vec<Retained>,
    ) -> io::Result<()> {
        let name = crate::QUARANTINE_LAYOUT;
        let path = dir.join(name);
        let mut current = match dir.open_file(name, true) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let disks = Disks::from([(label.to_string(), mount.to_string())]);
                return create(dir, &disks, retained);
            }
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(foreign(&path, ("символическая ссылка", "a symbolic link")))
            }
            Err(e) if e.raw_os_error() == Some(libc::EISDIR) => {
                return Err(foreign(&path, ("не обычный файл", "not a plain file")))
            }
            Err(e) => return Err(e),
        };
        let md = current.metadata()?;
        admit_existing(&path, &md)?;
        let mut raw = String::new();
        current.read_to_string(&mut raw)?;
        let (mut disks, versioned) = match parse(&raw) {
            Some(Parsed::Versioned(d)) => (d, true),
            Some(Parsed::Unversioned(d)) => (d, false),
            None => {
                return Err(foreign(
                    &path,
                    ("не раскладка карантина", "not a quarantine layout"),
                ))
            }
        };
        match disks.get(label) {
            Some(m) if m == mount => return Ok(()),
            Some(_) => {
                return Err(foreign(
                    &path,
                    (
                        "эта метка диска уже записана для другой точки монтирования",
                        "this disk label is already recorded for another mount point",
                    ),
                ))
            }
            None => {}
        }
        if !versioned {
            return Err(foreign(
                &path,
                (
                    "раскладка записана прежней версией без указания формата и не переписывается; проверьте её и уберите, или укажите другой каталог карантина",
                    "the layout was written by an earlier version without a stated format and is not rewritten; check it and move it aside, or give another quarantine folder",
                ),
            ));
        }
        disks.insert(label.to_string(), mount.to_string());
        let body = body(&disks)?;
        // Still the object at the name: the one read is the one written.
        if dir.stat_at(name)?.0 != ident_of(&md) {
            return Err(foreign(
                &path,
                ("заменён во время записи", "replaced while being read"),
            ));
        }
        admit_existing(&path, &current.metadata()?)?;
        current.seek(io::SeekFrom::Start(0))?;
        current.write_all(body.as_bytes())?;
        current.set_len(body.len() as u64)?;
        current.sync_all()
    }

    /// Provenance of an existing note: a plain file, one name, this user's.
    fn admit_existing(path: &Path, md: &std::fs::Metadata) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        if !md.is_file() {
            return Err(foreign(path, ("не обычный файл", "not a plain file")));
        }
        if md.nlink() != 1 {
            return Err(foreign(
                path,
                (
                    "у файла есть другое имя (жёсткая ссылка)",
                    "the file has another name (a hard link)",
                ),
            ));
        }
        // SAFETY: plain getter.
        if md.uid() != unsafe { libc::geteuid() } {
            return Err(foreign(
                path,
                (
                    "принадлежит другому пользователю",
                    "belongs to another user",
                ),
            ));
        }
        if md.len() > MAX_NOTE {
            return Err(foreign(
                path,
                ("не раскладка карантина", "not a quarantine layout"),
            ));
        }
        Ok(())
    }

    fn body(disks: &Disks) -> io::Result<String> {
        serde_json::to_string_pretty(&Versioned {
            format: FORMAT.into(),
            version: VERSION,
            disks: disks.clone(),
        })
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// A brand-new note: a fresh file, then a rename that replaces nothing,
    /// then a look at what was published.
    fn create(dir: &Dir, disks: &Disks, retained: &mut Vec<Retained>) -> io::Result<()> {
        let name = crate::QUARANTINE_LAYOUT;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = format!(".{name}.{}.{nanos}.tmp", std::process::id());
        let body = body(disks)?;
        #[cfg(test)]
        seam::fire_result(Stage::Create, &dir.join(&tmp))?;
        // Failing here grants no ownership of anything: whatever is at the
        // name is not ours, and nothing is cleaned up.
        let mut f = dir.create_new(&tmp, 0o644)?;
        let own = ident_of(&f.metadata()?);
        let written = (|| {
            f.write_all(body.as_bytes())?;
            #[cfg(test)]
            seam::fire_result(Stage::Written, &dir.join(&tmp))?;
            f.sync_all()
        })();
        if let Err(e) = written {
            retain(dir, &tmp, own, retained);
            return Err(e);
        }
        #[cfg(test)]
        let published = seam::fire_result(Stage::Publish, &dir.join(&tmp))
            .and_then(|()| dir.rename_no_replace(&tmp, name));
        #[cfg(not(test))]
        let published = dir.rename_no_replace(&tmp, name);
        if let Err(e) = published {
            retain(dir, &tmp, own, retained);
            return Err(if e.kind() == io::ErrorKind::AlreadyExists {
                foreign(
                    &dir.join(name),
                    (
                        "появился, пока писалась раскладка",
                        "appeared while the layout was written",
                    ),
                )
            } else {
                e
            });
        }
        // The rename checks only that the name was free. What it published
        // is whatever bore the temporary name at that instant.
        match dir.stat_at(name) {
            Ok((id, _)) if id == own => {
                // Durability of the name, best effort: the note is in place.
                let _ = dir.sync();
                Ok(())
            }
            _ => {
                retained.push(Retained {
                    path: dir.join(name),
                    why: crate::tr!(
                        "опубликован не тот файл, что записан этой операцией: временный файл подменили; чужой файл оставлен на месте",
                        "what was published is not the file this operation wrote: the temporary was substituted; that file is left in place"
                    )
                    .into(),
                });
                retained.push(Retained {
                    path: dir.join(&tmp),
                    why: crate::tr!(
                        "последнее известное имя записанного этой операцией файла; куда его переместили, неизвестно",
                        "last known name of the file this operation wrote; where it was moved is not known"
                    )
                    .into(),
                });
                Err(foreign(
                    &dir.join(name),
                    (
                        "подменён при публикации",
                        "substituted while being published",
                    ),
                ))
            }
        }
    }

    /// The temporary is never removed (user decision 2026-10-04, el-1y8uo
    /// B1): POSIX has no conditional unlink, and a stranger can take the
    /// name between any check and the removal. It is left where it is and
    /// reported; what bears the name now is looked at only to say so.
    fn retain(dir: &Dir, tmp: &str, own: Ident, retained: &mut Vec<Retained>) {
        let why = match dir.stat_at(tmp) {
            Ok((id, _)) if id == own => crate::tr!(
                "временный файл этой операции не опубликован и оставлен на месте: ничего не удаляется",
                "this operation's temporary file was not published and is left in place: nothing is removed"
            )
            .to_string(),
            Ok(_) => crate::tr!(
                "под этим именем теперь чужой файл, он оставлен на месте; файл, записанный этой операцией, перемещён неизвестно куда",
                "another file now has this name and is left in place; the file this operation wrote was moved to an unknown name"
            )
            .to_string(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => crate::tr!(
                "файл, записанный этой операцией, перемещён неизвестно куда",
                "the file this operation wrote was moved to an unknown name"
            )
            .to_string(),
            Err(e) => crate::tf!(
                "временный файл этой операции оставлен на месте; что под этим именем сейчас, прочитать не удалось: {0}",
                "this operation's temporary file is left in place; what bears the name now could not be read: {0}",
                e
            ),
        };
        retained.push(Retained {
            path: dir.join(tmp),
            why,
        });
    }
}

/// The path a file under a gathered quarantine came from.
///
/// `rest` is what lies below the gathered root: `<label>/<path from that
/// disk's mount>`. Without a note for that label there is no answer, and
/// inventing one is worse than saying so.
pub fn origin(disks: &Disks, rest: &Path) -> Option<PathBuf> {
    let mut parts = rest.components();
    let label = parts.next()?.as_os_str().to_str()?;
    let mount = disks.get(label)?;
    let tail = parts.as_path();
    if tail.as_os_str().is_empty() {
        return None;
    }
    Some(Path::new(mount).join(tail))
}

#[cfg(all(test, unix))]
#[path = "quarantine_layout_tests.rs"]
mod tests;

/// Windows (el-usdqi D4): the layout is not written there until a held
/// folder, file identity and link counts are verified on a real system.
/// Compiled and run only on Windows; not executed by the macOS/Linux gates.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::fs;

    /// A layout hard-linked to a file elsewhere is never extended through
    /// the link: nothing is written at all, the other name keeps its bytes.
    #[test]
    fn windows_layout_extension_does_not_write_a_foreign_hardlink() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("q");
        fs::create_dir(&root).unwrap();
        let other = tmp.path().join("someone-elses.json");
        let original = format!(r#"{{"format":"{FORMAT}","version":1,"disks":{{}}}}"#);
        fs::write(&other, &original).unwrap();
        fs::hard_link(&other, root.join(crate::QUARANTINE_LAYOUT)).unwrap();

        assert!(note(&root, "disk1", Path::new("D:\\")).is_err());
        assert_eq!(fs::read_to_string(&other).unwrap(), original);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    }
}
