//! The way home of an old row whose run recorded no roots (el-53l7h).
//!
//! Rows written before folder identities were recorded are checked up to
//! their own run's root ([`crate::recovery`], el-1nx1u B3). Runs started
//! from the web by older versions recorded no roots at all (`roots = []`):
//! such a row has neither an identity nor a bound, and after el-6s3go it
//! was refused for good — the files safe in quarantine, but no way to bring
//! them back from the tool.
//!
//! Its bound is the volume itself: the folder where the path enters the
//! volume the files were moved from (the device in their recorded
//! evidence) — its mount point, or on macOS the firmlinked `/Users`. From
//! there down to the folder the files left, every component is opened
//! through the folder above it, held, and never followed (`openat` with
//! `O_NOFOLLOW`): a symbolic link, a file or anything else in the place of a
//! folder is refused and named, and so is a folder on another volume. A
//! folder that is missing is made again — as earlier versions did — but
//! only through the held folder above it and never over anything, and only
//! once every member of the unit has been checked. The rename then goes into
//! the folder held, not into whatever its path names by then.
//!
//! Above the bound the path is resolved as the user's own: on macOS
//! `/var`, `/tmp` and `/etc` are links. Exactly this is allowed: a link on
//! the way to the mount point, or at it, only when it belongs to the system
//! (owned by root, as those are), and the folder reached must be the top of
//! that volume on its path — on the files' device, its parent (`..`) on
//! another one or the root itself. A link the user can make or replace is
//! refused there as anywhere below. A link to a folder *below* the mount
//! point (`/var/folders/...` when `/` and `/private/var` are one volume) is
//! below the bound and refused like any other.
//!
//! Unix only: on Windows no row was ever written without an identity.

use pc_core::anchored::Dir;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The checked way to a folder an old row comes back to: the deepest folder
/// that exists, held, and the names below it still to be made.
pub(crate) struct Chain {
    held: Dir,
    missing: Vec<String>,
    dev: u64,
}

/// `S_IFMT` and its values: the same on every Unix this builds for.
const IFMT: u32 = 0o170_000;
const IFLNK: u32 = 0o120_000;
const IFDIR: u32 = 0o040_000;

fn link_text() -> &'static str {
    pc_core::tr!(
        "символическая ссылка или соединение (junction)",
        "a symbolic link or a junction"
    )
}

fn other_text() -> &'static str {
    pc_core::tr!(
        "не папка (файл, облачная заглушка или иной особый объект)",
        "not a folder (a file, a cloud placeholder or another special object)"
    )
}

fn refuse(at: &Path, what: &str) -> String {
    pc_core::tf!(
        "место возврата {0} — {1}; программа не переходит по нему и ничего туда не возвращает",
        "the place to return to, {0}, is {1}; the tool does not follow it and returns nothing there",
        at.display(),
        what
    )
}

fn other_volume(at: &Path) -> String {
    refuse(
        at,
        pc_core::tr!(
            "папка на другом томе, не на том, откуда файлы перенесены",
            "a folder on another volume than the one the files were moved from"
        ),
    )
}

/// Check the way from the mount point of the volume `dev` to `folder`.
/// Nothing is created; an `Err` says, in words, which place is refused.
pub(crate) fn check(folder: &Path, dev: u64) -> Result<Chain, String> {
    let mut parts = folder.components();
    let names: Option<Vec<&str>> = if parts.next() == Some(Component::RootDir) {
        parts
            .map(|c| match c {
                Component::Normal(n) => n.to_str(),
                _ => None,
            })
            .collect()
    } else {
        None
    };
    let Some(names) = names else {
        return Err(refuse(
            folder,
            pc_core::tr!("не полный путь от корня", "not a full path from the root"),
        ));
    };
    // `/`, `/a`, `/a/b`, … `folder`.
    let mut prefixes = vec![PathBuf::from("/")];
    for n in &names {
        let next = prefixes.last().unwrap().join(n);
        prefixes.push(next);
    }
    // Where the path enters the volume: the shallowest folder on it that
    // is on the files' device, as the user's path resolves.
    let Some(top) = prefixes
        .iter()
        .position(|p| std::fs::metadata(p).is_ok_and(|m| m.dev() == dev))
    else {
        return Err(pc_core::tf!(
            "место возврата {0} не на томе, откуда файлы перенесены (том не подключён или путь ведёт на другой); программа ничего туда не возвращает",
            "the place to return to, {0}, is not on the volume the files were moved from (the volume is not mounted, or the path leads to another one); the tool returns nothing there",
            folder.display()
        ));
    };
    // Above the bound and at it, only the system's own links are followed.
    for p in &prefixes[..=top] {
        let md = std::fs::symlink_metadata(p).map_err(|e| refuse(p, &e.to_string()))?;
        if md.file_type().is_symlink() && md.uid() != 0 {
            return Err(refuse(p, link_text()));
        }
    }
    let bound = &prefixes[top];
    let mut held = Dir::open_following(bound).map_err(|e| refuse(bound, &e.to_string()))?;
    let id = held.ident().map_err(|e| refuse(bound, &e.to_string()))?;
    if id.0 != dev {
        return Err(other_volume(bound));
    }
    let up = held
        .parent_ident()
        .map_err(|e| refuse(bound, &e.to_string()))?;
    if up != id && up.0 == dev {
        return Err(refuse(
            bound,
            pc_core::tr!(
                "не точка подключения тома, а папка внутри него, достигнутая через ссылку",
                "not where its volume is mounted but a folder inside it, reached through a link"
            ),
        ));
    }
    let below = &names[top..];
    for (i, name) in below.iter().enumerate() {
        let at = held.join(name);
        match held.open_dir(name) {
            Ok(d) => {
                if d.ident().map_err(|e| refuse(&at, &e.to_string()))?.0 != dev {
                    return Err(other_volume(&at));
                }
                held = d;
            }
            Err(e) => {
                return match held.stat_at(name) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Chain {
                        held,
                        missing: below[i..].iter().map(|s| s.to_string()).collect(),
                        dev,
                    }),
                    Err(e) => Err(refuse(&at, &e.to_string())),
                    Ok((_, mode)) if mode & IFMT == IFLNK => Err(refuse(&at, link_text())),
                    Ok((_, mode)) if mode & IFMT == IFDIR => Err(refuse(&at, &e.to_string())),
                    Ok(_) => Err(refuse(&at, other_text())),
                };
            }
        }
    }
    Ok(Chain {
        held,
        missing: Vec::new(),
        dev,
    })
}

impl Chain {
    /// The folder itself: the folders still missing made, one by one,
    /// through the folder held above each, never over anything. One made
    /// meanwhile by someone else is used only as a real folder on the same
    /// volume, opened without following.
    pub(crate) fn hold(self) -> Result<Dir, String> {
        let mut held = self.held;
        for name in &self.missing {
            let at = held.join(name);
            let d = match held.mkdir(name) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    held.open_dir(name).map_err(|e| match held.stat_at(name) {
                        Ok((_, mode)) if mode & IFMT == IFLNK => refuse(&at, link_text()),
                        Ok((_, mode)) if mode & IFMT != IFDIR => refuse(&at, other_text()),
                        _ => refuse(&at, &e.to_string()),
                    })?
                }
                Err(e) => {
                    return Err(pc_core::tf!(
                        "не создать папку {0}: {1}; ничего туда не возвращено",
                        "cannot create the folder {0}: {1}; nothing was returned there",
                        at.display(),
                        e
                    ))
                }
            };
            if d.ident().map_err(|e| refuse(&at, &e.to_string()))?.0 != self.dev {
                return Err(other_volume(&at));
            }
            held = d;
        }
        Ok(held)
    }
}
