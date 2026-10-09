//! Content-addressed thumbnail cache.
//!
//! Kept outside SQLite so it can be copied, rsynced or thrown away on its
//! own, and so identical pixels stored under many paths cost one file.

use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ThumbStore {
    root: PathBuf,
    /// A bound data folder's proof ([`crate::storage`]), asked before every
    /// write and removal. `None` for an ordinary folder.
    binding: Option<crate::storage::Binding>,
}

pub fn hex32(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xF) as u32, 16).unwrap());
    }
    s
}

impl ThumbStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            binding: None,
        }
    }

    /// A cache in a bound data folder: nothing is written into or removed
    /// from `root` unless the binding confirms, right before, that it is
    /// still the proven folder. A replaced folder is left exactly as it is.
    pub fn bound(root: impl Into<PathBuf>, binding: crate::storage::Binding) -> Self {
        Self {
            root: root.into(),
            binding: Some(binding),
        }
    }

    /// For a bound cache: it is still the proven folder, so a change made
    /// now would land in it ([`crate::storage`]). Always `Ok` otherwise.
    /// Every write and removal asks this itself; callers that change other
    /// things along with the cache ask first, so a refusal changes nothing.
    pub fn confirm(&self) -> Result<()> {
        self.check()
    }

    fn check(&self) -> Result<()> {
        match &self.binding {
            None => Ok(()),
            Some(b) => b
                .check_thumbnails()
                .map_err(|why| crate::storage::NotBound(why).into()),
        }
    }

    /// `ab/cd/<key>.jpg` — two levels of fan-out keeps directories small
    /// enough that a filesystem listing stays usable.
    pub fn path_for(&self, key: &str) -> PathBuf {
        let (a, b) = (&key[0..2], &key[2..4]);
        self.root.join(a).join(b).join(format!("{key}.jpg"))
    }

    /// Store the bytes and return their key. Writing the same thumbnail twice
    /// is a no-op.
    pub fn put(&self, jpeg: &[u8]) -> Result<String> {
        // An encoder that gave up leaves an empty buffer. Stored, it becomes a
        // key that resolves to nothing, and the interface shows a grey square
        // where a photograph should be — which looks like a broken file
        // rather than a missing thumbnail.
        if jpeg.is_empty() {
            anyhow::bail!(crate::tr!(
                "пустая миниатюра",
                "the thumbnail came out empty"
            ));
        }
        let key = hex32(&blake3_of(jpeg)[..16]);
        if self.path_for(&key).exists() {
            return Ok(key);
        }
        self.write(&key, jpeg)?;
        Ok(key)
    }

    /// Store under a key of the caller's choosing, for things that are not
    /// identified by their own content — a rendered view of a file, which is
    /// keyed by the file it was rendered from. The key has the shape of a
    /// content key (32 lowercase hex digits) and is recorded like any other
    /// thumbnail, so [`ThumbStore::clear`] can prove it is the store's own.
    pub fn put_at(&self, key: &str, jpeg: &[u8]) -> Result<()> {
        if !is_key(key) {
            anyhow::bail!(crate::tf!(
                "недопустимый ключ миниатюры {0}",
                "not a thumbnail key: {0}",
                key
            ));
        }
        self.write(key, jpeg)
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        if key.len() < 4 {
            return None;
        }
        fs::read(self.path_for(key)).ok()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Empty the cache of what the store itself made, keeping the
    /// directory itself.
    ///
    /// Thumbnails are addressed by content, so nothing here is worth keeping
    /// once the rows that referenced them are gone — and a cache left behind
    /// after a reset is several gigabytes that no page will ever ask for.
    ///
    /// A name proves nothing: a file of this user's with a thumbnail's name,
    /// a temporary-looking name or a folder at `ab/cd` may be anybody's
    /// (review el-bdi66 B1). So the store records, when it creates a fan-out
    /// folder or publishes a thumbnail, the identity the object has right
    /// then (`MADE_RECORD` in the cache folder: device and inode, for a
    /// folder its birth time where the system keeps one, for a file its
    /// size, modification and change time), and a clear removes only what
    /// still has exactly that identity:
    ///
    /// - a fan-out folder is entered only if it is the one recorded, this
    ///   user's, not a link and closed to everybody else; any other folder
    ///   — a substitute, one made by a version that kept no record — is left
    ///   with everything in it and never changed (B2);
    /// - in it, a `<key>.jpg` goes only if, opened without following a link
    ///   and compared through that descriptor, it is a plain file of this
    ///   user's with one name and the recorded identity; then `unlinkat`
    ///   relative to the held folder, and the held descriptor confirms that
    ///   it was this file that went;
    /// - fan-out folders this left empty are removed (`rmdir` refuses one
    ///   that is not, and only the held, proven one is asked for).
    ///
    /// Everything else — another name, a temporary file, a link, a folder,
    /// a second name, somebody else's file, a replacement at a genuine
    /// name, the whole cache of a version before the record — stays where
    /// it is and is reported in [`Cleared::kept`]. A cache copied by a move
    /// of the data folder has new inodes, so it stays too: keeping too much
    /// costs space, never a file.
    ///
    /// The cache folder is opened once and everything happens through that
    /// descriptor. In a bound data folder the binding confirms that this
    /// descriptor is the proven folder ([`crate::storage`]) before anything
    /// goes: a replacement put at its path — somebody else's pictures, say,
    /// before the check or right after it — is never listed, let alone
    /// emptied.
    ///
    /// What a check cannot see is a name swapped between the comparison
    /// and the `unlinkat` that removes it. The proven folders are this
    /// user's and closed to everybody else (0700), so only a process of
    /// this user (or root) could do that, deliberately — outside the threat
    /// model ([`crate::storage`]); even then the held descriptor shows it,
    /// and the reply says so.
    pub fn clear(&self) -> Result<Cleared> {
        platform::clear(self)
    }

    /// Write `jpeg` as the thumbnail `key` (already validated).
    fn write(&self, key: &str, jpeg: &[u8]) -> Result<()> {
        platform::write(self, key, jpeg)
    }

    /// For a bound cache, the descriptor `folder` is the proven folder and
    /// the path still leads to it. Always `Ok` otherwise.
    #[cfg(unix)]
    fn check_folder(&self, folder: &fs::File) -> Result<()> {
        match &self.binding {
            None => Ok(()),
            Some(b) => b
                .check_thumbnail_folder(folder)
                .map_err(|why| crate::storage::NotBound(why).into()),
        }
    }
}

/// What [`ThumbStore::clear`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cleared {
    /// Thumbnails removed: each one proven, by its recorded identity, to
    /// be the file the store wrote.
    pub removed: u64,
    /// Entries left in place because they could not be proven to be the
    /// store's own, or could not be removed. A kept folder counts once,
    /// with everything in it.
    pub kept: u64,
    /// The first [`KEPT_EXAMPLES`] of those, with the reason.
    pub examples: Vec<(PathBuf, String)>,
}

/// How many kept entries a clear names; the count covers all of them.
pub const KEPT_EXAMPLES: usize = 20;

impl Cleared {
    #[cfg_attr(not(unix), allow(dead_code))]
    fn keep(&mut self, path: PathBuf, why: impl Into<String>) {
        self.kept += 1;
        if self.examples.len() < KEPT_EXAMPLES {
            self.examples.push((path, why.into()));
        }
    }
}

/// 32 lowercase hex digits: a key [`ThumbStore`] makes.
fn is_key(key: &str) -> bool {
    key.len() == 32 && is_hex(key)
}

fn is_hex(s: &str) -> bool {
    s.bytes()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Fan-out folders and thumbnails are made for this user only. The umask can
/// only take bits away from these, so a umask of 000 no longer leaves the
/// cache writable by everybody (el-5x1uh O3). Only what the store creates
/// gets these; an existing folder is never changed (B2).
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// The store's record of what it made, in the cache folder: one line per
/// fan-out folder it created and per thumbnail it wrote, with the identity
/// the object had right after (el-5x1uh B1, review el-bdi66). Only the
/// store appends to it; [`ThumbStore::clear`] removes nothing else.
#[cfg(unix)]
pub const MADE_RECORD: &str = ".thumbstore-made";

#[cfg(unix)]
mod platform {
    use super::{is_hex, is_key, Cleared, ThumbStore, DIR_MODE, FILE_MODE, MADE_RECORD};
    use crate::anchored::Dir;
    use anyhow::{Context, Result};
    use std::collections::HashMap;
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::Path;

    fn euid() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::geteuid() }
    }

    /// The cache folder, as the user configured it: a link at its own name
    /// is followed (the user may keep the cache elsewhere); everything below
    /// it is reached through descriptors and never through a link.
    fn open_root(store: &ThumbStore) -> io::Result<Dir> {
        Dir::open_following(&store.root)
    }

    /// What identifies a thumbnail the store wrote, read with `fstat` on
    /// the descriptor it wrote through, after the rename that published it.
    /// The change time cannot be set by anybody but the system, and moves
    /// on every rename, link, `chmod` or write: another file put at the
    /// name — even a copy of the same bytes, even the store's own file
    /// renamed away and back with a change in between — does not match.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FileMade {
        dev: u64,
        ino: u64,
        size: u64,
        mtime: (i64, i64),
        ctime: (i64, i64),
    }

    impl FileMade {
        fn of(md: &std::fs::Metadata) -> Self {
            Self {
                dev: md.dev(),
                ino: md.ino(),
                size: md.size(),
                mtime: (md.mtime(), md.mtime_nsec()),
                ctime: (md.ctime(), md.ctime_nsec()),
            }
        }
    }

    /// What identifies a fan-out folder the store created: device, inode
    /// and, where the system keeps one, the birth time (a folder's change
    /// time moves with every entry added, so it cannot be used).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct DirMade {
        dev: u64,
        ino: u64,
        born: Option<(u64, u32)>,
    }

    impl DirMade {
        fn of(md: &std::fs::Metadata) -> Self {
            let born = md
                .created()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| (d.as_secs(), d.subsec_nanos()));
            Self {
                dev: md.dev(),
                ino: md.ino(),
                born,
            }
        }

        /// `now` is the folder recorded as `self`. A recorded birth time
        /// the system no longer reports is no match.
        fn matches(&self, now: &DirMade) -> bool {
            self.dev == now.dev
                && self.ino == now.ino
                && (self.born.is_none() || self.born == now.born)
        }
    }

    /// The record, read back: relative name → what was made there (a name
    /// written several times has several entries; any may be the one there
    /// now).
    #[derive(Default)]
    struct Made {
        dirs: HashMap<String, Vec<DirMade>>,
        files: HashMap<String, Vec<FileMade>>,
    }

    impl Made {
        fn parse(text: &str) -> Self {
            let mut made = Made::default();
            for line in text.lines() {
                let f: Vec<&str> = line.split(' ').collect();
                let n = |i: usize| f.get(i).and_then(|s| s.parse::<i64>().ok());
                let u = |i: usize| f.get(i).and_then(|s| s.parse::<u64>().ok());
                match f.as_slice() {
                    ["d", rel, _, _, born_s, born_ns] => {
                        let (Some(dev), Some(ino)) = (u(2), u(3)) else {
                            continue;
                        };
                        let born = match (*born_s, *born_ns) {
                            ("-", "-") => None,
                            (s, ns) => match (s.parse(), ns.parse()) {
                                (Ok(s), Ok(ns)) => Some((s, ns)),
                                _ => continue,
                            },
                        };
                        made.dirs.entry(rel.to_string()).or_default().push(DirMade {
                            dev,
                            ino,
                            born,
                        });
                    }
                    ["f", rel, ..] if f.len() == 9 => {
                        let (Some(dev), Some(ino), Some(size)) = (u(2), u(3), u(4)) else {
                            continue;
                        };
                        let (Some(ms), Some(mn), Some(cs), Some(cn)) = (n(5), n(6), n(7), n(8))
                        else {
                            continue;
                        };
                        made.files
                            .entry(rel.to_string())
                            .or_default()
                            .push(FileMade {
                                dev,
                                ino,
                                size,
                                mtime: (ms, mn),
                                ctime: (cs, cn),
                            });
                    }
                    _ => {}
                }
            }
            made
        }

        fn dir(&self, rel: &str, now: &DirMade) -> bool {
            self.dirs
                .get(rel)
                .is_some_and(|v| v.iter().any(|m| m.matches(now)))
        }

        fn file(&self, rel: &str, now: &FileMade) -> bool {
            self.files.get(rel).is_some_and(|v| v.contains(now))
        }
    }

    /// Append one line to the record in the held cache folder. One
    /// `write` per line under a lock, so lines of this process never
    /// interleave; a line that does not parse is ignored when read, which
    /// only means its object is kept.
    fn record(root: &Dir, line: &str) -> io::Result<()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut file = root.open_append(MADE_RECORD, FILE_MODE)?;
        let md = file.metadata()?;
        if !md.is_file() || md.uid() != euid() || md.nlink() != 1 {
            return Err(io::Error::other(crate::tf!(
                "{0} — не файл записи кэша (не обычный файл этого пользователя с одним именем)",
                "{0} is not the cache's record (not a plain file of this user's with one name)",
                root.join(MADE_RECORD).display()
            )));
        }
        file.write_all(line.as_bytes())
    }

    fn record_dir(root: &Dir, rel: &str, dir: &Dir) -> io::Result<()> {
        let made = DirMade::of(&dir.file().metadata()?);
        let born = match made.born {
            Some((s, ns)) => format!("{s} {ns}"),
            None => "- -".to_string(),
        };
        record(root, &format!("d {rel} {} {} {born}\n", made.dev, made.ino))
    }

    fn record_file(root: &Dir, rel: &str, file: &std::fs::File) -> io::Result<()> {
        let m = FileMade::of(&file.metadata()?);
        record(
            root,
            &format!(
                "f {rel} {} {} {} {} {} {} {}\n",
                m.dev, m.ino, m.size, m.mtime.0, m.mtime.1, m.ctime.0, m.ctime.1
            ),
        )
    }

    /// Open the fan-out folder `name` in `parent` (making it with
    /// [`DIR_MODE`] first if `create`). It must be a real folder, not a
    /// link, and this user's. Returns whether this call created it.
    ///
    /// An existing folder is never changed — not its mode, not anything
    /// else (el-5x1uh B2): nothing proves it is one the store made. Only a
    /// folder made right here gets the explicit [`DIR_MODE`].
    fn fan_out(parent: &Dir, name: &str, create: bool) -> std::result::Result<(Dir, bool), String> {
        let mut created = false;
        if create {
            match parent.mkdir_mode(name, DIR_MODE) {
                Ok(()) => created = true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        let dir = parent.open_dir(name).map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => crate::tr!(
                "не каталог (ссылка или файл)",
                "not a folder (a link or a file)"
            )
            .to_string(),
            _ => e.to_string(),
        })?;
        let md = dir.file().metadata().map_err(|e| e.to_string())?;
        if md.uid() != euid() {
            return Err(crate::tf!(
                "каталог принадлежит другому пользователю (uid {0})",
                "the folder belongs to another user (uid {0})",
                md.uid()
            ));
        }
        Ok((dir, created))
    }

    pub(super) fn write(store: &ThumbStore, key: &str, jpeg: &[u8]) -> Result<()> {
        let root = match open_root(store) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound && store.binding.is_none() => {
                // An ordinary cache is made on first use; a bound one exists
                // since the move and is never re-created by path.
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(DIR_MODE)
                    .create(&store.root)
                    .with_context(|| {
                        crate::tf!("не создать {0}", "cannot create {0}", store.root.display())
                    })?;
                open_root(store)?
            }
            Err(e) => {
                store.check()?;
                return Err(e).with_context(|| {
                    crate::tf!("не открыть {0}", "cannot open {0}", store.root.display())
                });
            }
        };
        store.check_folder(root.file())?;
        let (a, b) = (&key[0..2], &key[2..4]);
        let refused = |path: &Path, why: String| {
            anyhow::anyhow!(crate::tf!(
                "{0}: миниатюра не записана: {1}",
                "{0}: the thumbnail was not written: {1}",
                path.display(),
                why
            ))
        };
        let unrecorded = |path: &Path, e: io::Error| {
            anyhow::anyhow!(crate::tf!(
                "{0}: создано кэшем, но не записано в его учёт ({1}) — сброс это оставит",
                "{0}: made by the cache but not entered in its record ({1}) — a reset will keep it",
                path.display(),
                e
            ))
        };
        let (first, made) = fan_out(&root, a, true).map_err(|why| refused(&root.join(a), why))?;
        if made {
            record_dir(&root, a, &first).map_err(|e| unrecorded(&root.join(a), e))?;
        }
        let (fan, made) = fan_out(&first, b, true).map_err(|why| refused(&first.join(b), why))?;
        let rel = format!("{a}/{b}");
        if made {
            record_dir(&root, &rel, &fan).map_err(|e| unrecorded(&first.join(b), e))?;
        }
        let name = format!("{key}.jpg");
        let file = write_then_rename(&fan, &name, key, jpeg)?;
        record_file(&root, &format!("{rel}/{name}"), &file)
            .map_err(|e| unrecorded(&fan.join(&name), e))
    }

    /// Write beside the target and rename onto it, so a crash never leaves a
    /// half-written file that later looks valid. Returns the descriptor the
    /// bytes were written through, for the record.
    ///
    /// The temporary name is unique per writer: several frames with identical
    /// content are hashed to one key and may be stored at the same moment, and a
    /// shared `.tmp` name means one writer renaming another writer's half-written
    /// file onto the target. Both ways into the store use this — they used to
    /// differ, and only one of them was safe.
    ///
    /// The temporary file is created new (`O_EXCL|O_NOFOLLOW`, [`FILE_MODE`])
    /// in the held fan-out folder; if the rename fails it is removed only if
    /// its name still bears the file this call created.
    fn write_then_rename(fan: &Dir, name: &str, key: &str, jpeg: &[u8]) -> Result<std::fs::File> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = format!("{key}.{}.{sequence}.tmp", std::process::id());
        let mut file = fan.create_new(&tmp, FILE_MODE).with_context(|| {
            crate::tf!(
                "не создать {0}",
                "cannot create {0}",
                fan.join(&tmp).display()
            )
        })?;
        let made = crate::anchored::ident_of(&file.metadata()?);
        let written = file
            .write_all(jpeg)
            .and_then(|()| fan.rename_replacing(&tmp, name));
        if let Err(e) = written {
            if fan.entry_at(&tmp).ok().map(|en| en.ident) == Some(made) {
                let _ = fan.remove_file_at(&tmp);
            }
            return Err(e).with_context(|| {
                crate::tf!(
                    "не записать {0}",
                    "cannot write {0}",
                    fan.join(name).display()
                )
            });
        }
        Ok(file)
    }

    /// The record as the cache folder holds it: nothing if there is none;
    /// if something else bears its name (a link, a folder, a file of
    /// another user's or with a second name), nothing either — so nothing
    /// is proven and nothing goes — and that entry is reported.
    fn read_record(root: &Dir, out: &mut Cleared) -> Made {
        let refuse = |out: &mut Cleared, why: String| {
            out.keep(root.join(MADE_RECORD), why);
            Made::default()
        };
        let entry = match root.entry_at(MADE_RECORD) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Made::default(),
            Err(e) => return refuse(out, e.to_string()),
        };
        if !entry.is_file() || entry.uid != euid() || entry.nlink != 1 {
            return refuse(out, record_refused());
        }
        let mut text = String::new();
        let read = root.open_file(MADE_RECORD, false).and_then(|mut f| {
            let md = f.metadata()?;
            if crate::anchored::ident_of(&md) != entry.ident {
                return Err(io::Error::other(record_refused()));
            }
            f.read_to_string(&mut text)
        });
        match read {
            Ok(_) => Made::parse(&text),
            Err(e) => refuse(out, e.to_string()),
        }
    }

    fn record_refused() -> String {
        crate::tr!(
            "под именем учёта кэша не обычный файл этого пользователя с одним именем — ничего не удалено",
            "the cache's record is not a plain file of this user's with one name — nothing was removed"
        )
        .to_string()
    }

    pub(super) fn clear(store: &ThumbStore) -> Result<Cleared> {
        let root = match open_root(store) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Never created, or already gone: nothing to clear — unless
                // it is a bound cache, which must be there.
                store.check()?;
                return Ok(Cleared::default());
            }
            Err(e) => {
                store.check()?;
                return Err(e).context(crate::tr!(
                    "не прочитать кэш превью",
                    "cannot read the thumbnail cache"
                ));
            }
        };
        store.check_folder(root.file())?;
        let mut out = Cleared::default();
        let made = read_record(&root, &mut out);
        let read = |dir: &Dir| {
            dir.names().with_context(|| {
                crate::tf!("не прочитать {0}", "cannot read {0}", dir.path().display())
            })
        };
        for first in read(&root)? {
            if first == MADE_RECORD {
                // Kept, and already reported above if it is not the record.
                continue;
            }
            let Some(a) = fan_out_name(&first) else {
                out.keep(root.path().join(&first), not_ours());
                continue;
            };
            let dir_a = match proven_fan_out(&root, a, a, &made) {
                Ok(d) => d,
                Err(why) => {
                    out.keep(root.join(a), why);
                    continue;
                }
            };
            for second in read(&dir_a)? {
                let Some(b) = fan_out_name(&second) else {
                    out.keep(dir_a.path().join(&second), not_ours());
                    continue;
                };
                let rel = format!("{a}/{b}");
                let dir_b = match proven_fan_out(&dir_a, b, &rel, &made) {
                    Ok(d) => d,
                    Err(why) => {
                        out.keep(dir_a.join(b), why);
                        continue;
                    }
                };
                let prefix = format!("{a}{b}");
                for name in read(&dir_b)? {
                    let path = dir_b.path().join(&name);
                    let Some(n) = name.to_str().filter(|n| is_thumbnail_name(n, &prefix)) else {
                        out.keep(path, not_ours());
                        continue;
                    };
                    match remove_made_file(&dir_b, n, &format!("{rel}/{n}"), &made) {
                        Ok(()) => out.removed += 1,
                        Err(why) => out.keep(path, why),
                    }
                }
                remove_if_empty(&dir_a, b, &dir_b);
            }
            remove_if_empty(&root, a, &dir_a);
        }
        Ok(out)
    }

    /// Open the fan-out folder `name` of `parent` (recorded as `rel`) for
    /// clearing: only one the store created, by the identity recorded then
    /// — this user's, not a link, still closed to everybody else. Anything
    /// else is left as it is, with its contents, and not changed in any
    /// way.
    fn proven_fan_out(
        parent: &Dir,
        name: &str,
        rel: &str,
        made: &Made,
    ) -> std::result::Result<Dir, String> {
        let (dir, _) = fan_out(parent, name, false)?;
        let md = dir.file().metadata().map_err(|e| e.to_string())?;
        if !made.dir(rel, &DirMade::of(&md)) {
            return Err(crate::tr!(
                "папка не создана кэшем миниатюр (нет записанной при создании идентичности) — оставлена со всем содержимым",
                "a folder the thumbnail cache did not create (no identity recorded when it was made) — left with everything in it"
            )
            .to_string());
        }
        if md.mode() & 0o077 != 0 {
            return Err(crate::tf!(
                "папка кэша открыта для других (права {0:o}) — оставлена со всем содержимым",
                "the cache's folder is open to others (mode {0:o}) — left with everything in it",
                md.mode() & 0o777
            ));
        }
        Ok(dir)
    }

    fn not_ours() -> String {
        crate::tr!(
            "не создано кэшем миниатюр",
            "not something the thumbnail cache creates"
        )
        .to_string()
    }

    /// Two lowercase hex digits.
    fn fan_out_name(name: &std::ffi::OsStr) -> Option<&str> {
        name.to_str().filter(|n| n.len() == 2 && is_hex(n))
    }

    /// `<key>.jpg` for a key in the fan-out folder `prefix`. A temporary
    /// name is never recorded, so it is never removed.
    fn is_thumbnail_name(name: &str, prefix: &str) -> bool {
        name.split_at_checked(32)
            .is_some_and(|(key, rest)| is_key(key) && key.starts_with(prefix) && rest == ".jpg")
    }

    /// Remove `name` from the held fan-out folder only if it is the very
    /// file the store wrote there: a plain file of this user's with a single
    /// name whose identity — device, inode, size, modification and change
    /// time — is the one recorded when the store published it. The file is
    /// opened (no link followed) and compared through that descriptor;
    /// after `unlinkat`, the held descriptor says whether it was the one
    /// that went.
    fn remove_made_file(
        dir: &Dir,
        name: &str,
        rel: &str,
        made: &Made,
    ) -> std::result::Result<(), String> {
        let entry = dir.entry_at(name).map_err(|e| e.to_string())?;
        if !entry.is_file() {
            return Err(crate::tr!(
                "не обычный файл (ссылка или каталог)",
                "not a plain file (a link or a folder)"
            )
            .into());
        }
        if entry.uid != euid() {
            return Err(crate::tf!(
                "файл принадлежит другому пользователю (uid {0})",
                "the file belongs to another user (uid {0})",
                entry.uid
            ));
        }
        if entry.nlink != 1 {
            return Err(crate::tf!(
                "у файла {0} имён — он есть где-то ещё",
                "the file has {0} names — it is somewhere else too",
                entry.nlink
            ));
        }
        let held = dir.open_file(name, false).map_err(|e| e.to_string())?;
        let md = held.metadata().map_err(|e| e.to_string())?;
        let now = FileMade::of(&md);
        if (now.dev, now.ino) != entry.ident || !md.is_file() || !made.file(rel, &now) {
            return Err(crate::tr!(
                "не та миниатюра, которую записал кэш (идентичность не совпадает с записанной)",
                "not the thumbnail the cache wrote (its identity is not the one recorded)"
            )
            .into());
        }
        dir.remove_file_at(name).map_err(|e| e.to_string())?;
        match held.metadata() {
            Ok(after) if after.nlink() == 0 => Ok(()),
            _ => Err(crate::tr!(
                "в момент удаления под этим именем было уже другое — удалено оно, записанная миниатюра осталась",
                "something else bore the name at the moment of removal and went instead; the recorded thumbnail stayed"
            )
            .into()),
        }
    }

    /// `rmdir` the fan-out folder `name` of `parent` if it is still the one
    /// held as `held` (proven by [`proven_fan_out`]); the system refuses it
    /// if anything is left in it.
    fn remove_if_empty(parent: &Dir, name: &str, held: &Dir) {
        if let (Ok(now), Ok(ident)) = (parent.entry_at(name), held.ident()) {
            if now.is_dir() && now.ident == ident {
                let _ = parent.remove_dir_at(name);
            }
        }
    }
}

/// Platforms without descriptor-relative operations: thumbnails are written
/// by path as before (the bound check by path is all there is), and the
/// cache is never cleared — nothing there can prove that what is under the
/// name is the store's own.
#[cfg(not(unix))]
mod platform {
    use super::{Cleared, ThumbStore};
    use anyhow::{Context, Result};
    use std::fs;

    pub(super) fn write(store: &ThumbStore, key: &str, jpeg: &[u8]) -> Result<()> {
        store.check()?;
        let path = store.path_for(key);
        let parent = path.parent().context(crate::tr!(
            "нет родительского каталога",
            "no parent directory"
        ))?;
        fs::create_dir_all(parent)
            .with_context(|| crate::tf!("не создать {0}", "cannot create {0}", parent.display()))?;
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("{}.{sequence}.tmp", std::process::id()));
        fs::write(&tmp, jpeg)?;
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
        Ok(())
    }

    pub(super) fn clear(store: &ThumbStore) -> Result<Cleared> {
        store.check()?;
        if !store.root.exists() {
            return Ok(Cleared::default());
        }
        anyhow::bail!(crate::tf!(
            "на этой платформе кэш превью не очищается: {0} оставлен как есть",
            "the thumbnail cache is not cleared on this platform: {0} is left as it is",
            store.root.display()
        ))
    }
}

fn blake3_of(bytes: &[u8]) -> [u8; 32] {
    // Duplicated rather than depending on pc-hash: this crate sits below it.
    let mut h = Hasher::new();
    h.update(bytes);
    h.finish()
}

/// Minimal FNV-style fallback is not good enough for addressing content, so
/// this is a thin shim over the same BLAKE3 the rest of the tool uses.
struct Hasher(blake3::Hasher);

impl Hasher {
    fn new() -> Self {
        Self(blake3::Hasher::new())
    }
    fn update(&mut self, b: &[u8]) {
        self.0.update(b);
    }
    fn finish(&self) -> [u8; 32] {
        *self.0.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn one_key_written_from_several_threads_at_once_stays_whole() {
        // A rendered view is keyed by the file it came from, so two tabs
        // opening the same frame store the same key at the same moment. With
        // a temporary name shared by both writers, one renames the other's
        // half-written file onto the target — or finds nothing to rename.
        let tmp = tempfile::tempdir().unwrap();
        let store = ThumbStore::new(tmp.path().to_path_buf());
        let key = "0123456789abcdef0123456789abcdef";
        let payloads: Vec<Vec<u8>> = (0..8u8).map(|n| vec![n; 4096]).collect();

        std::thread::scope(|scope| {
            for jpeg in &payloads {
                scope.spawn(|| store.put_at(key, jpeg).expect("запись не удалась"));
            }
        });

        let stored = store.get(key).expect("ничего не сохранилось");
        assert!(
            payloads.contains(&stored),
            "сохранилось не то, что писали: {} байт",
            stored.len()
        );
    }

    use super::*;

    #[test]
    fn stores_and_reads_back() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        let key = s.put(b"pretend jpeg").unwrap();
        assert_eq!(s.get(&key).unwrap(), b"pretend jpeg");
    }

    #[test]
    fn identical_content_shares_one_file() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        let a = s.put(b"same").unwrap();
        let b = s.put(b"same").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, s.put(b"different").unwrap());
    }

    #[test]
    fn concurrent_duplicates_all_receive_a_thumbnail_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(ThumbStore::new(tmp.path()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(24));
        let workers: Vec<_> = (0..24)
            .map(|_| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.put(&vec![37u8; 100_000]).unwrap()
                })
            })
            .collect();
        let keys: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert!(keys.iter().all(|k| k == &keys[0]));
        assert_eq!(store.get(&keys[0]).unwrap().len(), 100_000);
    }

    #[test]
    fn clearing_removes_every_thumbnail_and_leaves_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        let keys: Vec<_> = (0..5)
            .map(|i| s.put(format!("thumb {i}").as_bytes()).unwrap())
            .collect();
        assert_eq!(
            s.clear().unwrap(),
            Cleared {
                removed: 5,
                ..Default::default()
            }
        );
        assert!(tmp.path().is_dir());
        assert!(keys.iter().all(|k| s.get(k).is_none()));
    }

    #[test]
    fn clearing_a_cache_that_was_never_written_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path().join("никогда-не-было"));
        assert_eq!(s.clear().unwrap(), Cleared::default());
    }

    #[test]
    fn a_missing_or_malformed_key_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        assert!(s.get("deadbeefdeadbeef").is_none());
        assert!(s.get("x").is_none());
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        s.put(b"content").unwrap();
        let leftovers = walkdir::WalkDir::new(tmp.path())
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}

/// What `clear` may and may not take (el-5x1uh C2, O3).
#[cfg(all(test, unix))]
mod ownership_tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn ident(p: &Path) -> (u64, u64) {
        let m = fs::symlink_metadata(p).unwrap();
        (m.dev(), m.ino())
    }

    /// Path, kind and bytes of everything under `dir`, links not followed.
    fn tree(dir: &Path) -> Vec<(String, String)> {
        let mut out: Vec<_> = walkdir::WalkDir::new(dir)
            .min_depth(1)
            .into_iter()
            .map(|e| {
                let e = e.unwrap();
                let p = e.path();
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                let t = e.file_type();
                let what = if t.is_symlink() {
                    format!("link -> {}", fs::read_link(p).unwrap().display())
                } else if t.is_dir() {
                    "dir".into()
                } else {
                    format!("{:?}", fs::read(p).unwrap())
                };
                (rel, what)
            })
            .collect();
        out.sort();
        out
    }

    /// A thumbnail-shaped name for the fan-out folder `ab/cd`.
    fn shaped(prefix: &str, tail: char) -> String {
        format!("{prefix}{}.jpg", tail.to_string().repeat(28))
    }

    /// Everything planted in or through the cache that the store did not
    /// write survives a clear; only the store's own five thumbnails go.
    #[test]
    fn clearing_takes_only_the_thumbnails_the_store_wrote() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("photo-41.jpg"), b"outside photo 41").unwrap();
        fs::create_dir(outside.join("cd")).unwrap();
        fs::write(outside.join("cd").join(shaped("efcd", '7')), b"outside 7").unwrap();

        let s = ThumbStore::new(&root);
        let keys: Vec<_> = (0..5)
            .map(|i| s.put(format!("thumb {i} 9137").as_bytes()).unwrap())
            .collect();
        let (a, b) = (&keys[0][0..2], &keys[0][2..4]);
        let fan = root.join(a).join(b);
        // A file and a folder of somebody else's at the top.
        fs::write(root.join("notes.txt"), b"notes 5521").unwrap();
        fs::create_dir(root.join("zz")).unwrap();
        fs::write(root.join("zz").join("kept.jpg"), b"kept 8812").unwrap();
        // Inside a genuine fan-out folder: a foreign name, a link and a
        // second name of an outside file under thumbnail-shaped names, and a
        // folder under a thumbnail-shaped name.
        fs::write(fan.join("foreign.jpg"), b"foreign 3301").unwrap();
        symlink(
            outside.join("photo-41.jpg"),
            fan.join(shaped(&format!("{a}{b}"), '1')),
        )
        .unwrap();
        fs::hard_link(
            outside.join("photo-41.jpg"),
            fan.join(shaped(&format!("{a}{b}"), '2')),
        )
        .unwrap();
        fs::create_dir(fan.join(shaped(&format!("{a}{b}"), '3'))).unwrap();
        // A fan-out-shaped name that is a link to an outside folder.
        let link_fan = if a == "ef" { "fe" } else { "ef" };
        symlink(&outside, root.join(link_fan)).unwrap();

        let before_outside = tree(&outside);
        let ab = format!("{a}/{b}");
        let planted: Vec<String> = vec![
            "notes.txt".into(),
            "zz".into(),
            "zz/kept.jpg".into(),
            format!("{ab}/foreign.jpg"),
            format!("{ab}/{}", shaped(&format!("{a}{b}"), '1')),
            format!("{ab}/{}", shaped(&format!("{a}{b}"), '2')),
            format!("{ab}/{}", shaped(&format!("{a}{b}"), '3')),
            link_fan.into(),
        ];
        let before = tree(&root);
        let planted: Vec<_> = before
            .iter()
            .filter(|e| planted.contains(&e.0))
            .cloned()
            .collect();
        assert_eq!(planted.len(), 8);

        let cleared = s.clear().unwrap();

        assert_eq!(cleared.removed, 5, "{cleared:?}");
        // zz (one entry, not walked into), notes.txt, the link at a fan-out
        // name, and four entries in the fan-out folder.
        assert_eq!(cleared.kept, 7, "{cleared:?}");
        assert!(
            keys.iter().all(|k| s.get(k).is_none()),
            "a thumbnail stayed"
        );
        assert_eq!(tree(&outside), before_outside, "the outside changed");
        let after = tree(&root);
        for p in &planted {
            assert!(after.contains(p), "{p:?} went; left: {after:?}");
        }
        // Only the planted entries, the fan-out folders holding them and
        // the store's record are left.
        assert_eq!(after.len(), planted.len() + 3, "{after:?}");
        assert!(after.iter().any(|e| e.0 == MADE_RECORD), "{after:?}");
        assert_eq!(
            fs::read(outside.join("photo-41.jpg")).unwrap(),
            b"outside photo 41"
        );
    }

    /// The cache's path is replaced right after the binding confirmed it
    /// (by the binding itself, the narrowest place a test can reach). The
    /// replacement — even with thumbnail-shaped entries — keeps every
    /// entry; the proven cache, moved aside, is the one cleared.
    #[test]
    fn a_cache_replaced_right_after_the_check_keeps_every_entry() {
        #[derive(Debug)]
        struct SwapAfterCheck {
            root: PathBuf,
            saved: PathBuf,
            proven: (u64, u64),
            swapped: AtomicBool,
        }
        impl crate::storage::StorageBinding for SwapAfterCheck {
            fn check_database(&self) -> std::result::Result<(), String> {
                Ok(())
            }
            fn check_thumbnails(&self) -> std::result::Result<(), String> {
                if ident(&self.root) != self.proven {
                    return Err("replaced".into());
                }
                if !self.swapped.swap(true, Ordering::SeqCst) {
                    fs::rename(&self.root, &self.saved).unwrap();
                    let fan = self.root.join("ab").join("cd");
                    fs::create_dir_all(&fan).unwrap();
                    fs::write(fan.join(shaped("abcd", '5')), b"foreign thumbnail 6229").unwrap();
                    fs::write(self.root.join("foreign.txt"), b"foreign 6230").unwrap();
                }
                Ok(())
            }
            fn check_thumbnail_folder(&self, folder: &fs::File) -> std::result::Result<(), String> {
                self.check_thumbnails()?;
                let m = folder.metadata().unwrap();
                if (m.dev(), m.ino()) == self.proven {
                    Ok(())
                } else {
                    Err("another folder".into())
                }
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let keys: Vec<_> = {
            let plain = ThumbStore::new(&root);
            (0..3)
                .map(|i| plain.put(format!("own {i} 4417").as_bytes()).unwrap())
                .collect()
        };
        let binding = Arc::new(SwapAfterCheck {
            root: root.clone(),
            saved: tmp.path().join("saved-thumbs"),
            proven: ident(&root),
            swapped: AtomicBool::new(false),
        });
        let s = ThumbStore::bound(&root, binding.clone());
        let _ = s.clear();
        assert!(binding.swapped.load(Ordering::SeqCst));
        assert_eq!(
            fs::read(root.join("ab/cd").join(shaped("abcd", '5'))).unwrap(),
            b"foreign thumbnail 6229"
        );
        assert_eq!(fs::read(root.join("foreign.txt")).unwrap(), b"foreign 6230");
        let saved = ThumbStore::new(tmp.path().join("saved-thumbs"));
        assert!(
            keys.iter().all(|k| saved.get(k).is_none()),
            "the proven cache was not cleared"
        );
    }

    /// A fan-out name that is a link: nothing is written through it.
    #[test]
    fn a_thumbnail_is_not_written_through_a_linked_fan_out_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&root).unwrap();
        let jpeg = b"linked fan-out 7781";
        let key = hex32(&blake3_of(jpeg)[..16]);
        symlink(&outside, root.join(&key[0..2])).unwrap();
        assert!(ThumbStore::new(&root).put(jpeg).is_err());
        assert!(ThumbStore::new(&root).put_at(&key, jpeg).is_err());
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            0,
            "written through the link"
        );
    }

    /// `umask` is process-wide, so the store runs under umask 000 in a child
    /// copy of the test binary. Fan-out folders come out 0700 and
    /// thumbnails 0600, not 0777/0666. A fan-out folder that already exists
    /// — here one an older version left 0777 — is not the store's to
    /// change (el-5x1uh B2): it keeps its mode, and only what the store
    /// creates in it is 0700/0600.
    #[test]
    fn under_umask_000_the_cache_is_not_left_writable_by_others() {
        const CHILD: &str = "PC_CORE_THUMBS_UMASK_DIR";
        if let Some(dir) = std::env::var_os(CHILD) {
            // SAFETY: umask has no preconditions; this child runs only this
            // test, on one thread.
            unsafe { libc::umask(0) };
            let root = Path::new(&dir).join("thumbs");
            let s = ThumbStore::new(&root);
            let key = s.put(b"umask zero 2207").unwrap();
            let legacy = root.join("9e");
            fs::create_dir(&legacy).unwrap();
            fs::set_permissions(&legacy, fs::Permissions::from_mode(0o777)).unwrap();
            let key2 = format!("9e{}", "4".repeat(30));
            s.put_at(&key2, b"legacy 2208").unwrap();
            fs::write(Path::new(&dir).join("keys"), format!("{key}\n{key2}")).unwrap();
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "thumbstore::ownership_tests::under_umask_000_the_cache_is_not_left_writable_by_others",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD, tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let keys = fs::read_to_string(tmp.path().join("keys")).unwrap();
        let root = tmp.path().join("thumbs");
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(root.join(MADE_RECORD)), 0o600);
        for key in keys.lines() {
            let (a, b) = (&key[0..2], &key[2..4]);
            let made = if a == "9e" { 0o777 } else { 0o700 };
            assert_eq!(mode(root.join(a)), made, "{a}");
            assert_eq!(mode(root.join(a).join(b)), 0o700, "{a}/{b}");
            assert_eq!(
                mode(root.join(a).join(b).join(format!("{key}.jpg"))),
                0o600,
                "{key}"
            );
        }
    }

    /// A cache a version without the record left (or one copied by a move
    /// of the data folder): nothing in it can be proven, so a clear keeps
    /// it whole — every folder, every file, every mode — and says so.
    #[test]
    fn a_cache_without_a_record_is_kept_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let fan = root.join("ab/cd");
        fs::create_dir_all(&fan).unwrap();
        fs::set_permissions(root.join("ab"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(fan.join(shaped("abcd", '6')), b"legacy thumbnail 5123").unwrap();
        let before = tree(&root);
        let mode_before = fs::metadata(root.join("ab")).unwrap().mode();

        let cleared = ThumbStore::new(&root).clear().unwrap();

        assert_eq!((cleared.removed, cleared.kept), (0, 1), "{cleared:?}");
        assert_eq!(cleared.examples[0].0, root.join("ab"));
        assert_eq!(tree(&root), before);
        assert_eq!(fs::metadata(root.join("ab")).unwrap().mode(), mode_before);
    }

    /// The store's thumbnail renamed away and another file put at its name
    /// — the same bytes, a temporary-looking name beside it, or the same
    /// file after a `chmod` — is not what the store wrote, and stays.
    #[test]
    fn a_replacement_at_a_genuine_name_is_kept_even_with_the_same_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let s = ThumbStore::new(&root);
        let copied = s.put(b"own 8120").unwrap();
        let touched = s.put(b"own 8121").unwrap();
        let gone = s.put(b"own 8122").unwrap();
        // The same bytes, a new file.
        let leaf = s.path_for(&copied);
        let bytes = fs::read(&leaf).unwrap();
        fs::rename(&leaf, tmp.path().join("saved-8120")).unwrap();
        fs::write(&leaf, &bytes).unwrap();
        // The store's own file, changed afterwards.
        fs::set_permissions(s.path_for(&touched), fs::Permissions::from_mode(0o640)).unwrap();
        // A temporary-shaped name the store never made.
        let tmp_name = s
            .path_for(&gone)
            .with_file_name(format!("{gone}.4242.7.tmp"));
        fs::write(&tmp_name, b"foreign temporary 8123").unwrap();

        let cleared = s.clear().unwrap();

        assert_eq!((cleared.removed, cleared.kept), (1, 3), "{cleared:?}");
        assert!(s.get(&gone).is_none());
        assert_eq!(fs::read(&leaf).unwrap(), bytes);
        assert_eq!(s.get(&touched).unwrap(), b"own 8121");
        assert_eq!(fs::read(&tmp_name).unwrap(), b"foreign temporary 8123");
    }

    /// A substituted fan-out folder — this user's, 0700, holding a file at
    /// a thumbnail's name — is not entered, not emptied, not changed; nor is
    /// one at 0750 narrowed.
    #[test]
    fn a_substituted_fan_out_folder_is_left_exactly_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let s = ThumbStore::new(&root);
        s.put_at(&format!("abcd{}", "0".repeat(28)), b"own 3310")
            .unwrap();
        let other = s.put(b"own 3311").unwrap();
        let fan = root.join("ab/cd");
        fs::rename(&fan, tmp.path().join("saved-fan")).unwrap();
        fs::create_dir(&fan).unwrap();
        fs::set_permissions(&fan, fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(fan.join(shaped("abcd", '0')), b"foreign 3312").unwrap();
        let before = tree(&fan);

        let cleared = s.clear().unwrap();

        assert_eq!((cleared.removed, cleared.kept), (1, 1), "{cleared:?}");
        assert!(s.get(&other).is_none());
        assert_eq!(cleared.examples[0].0, fan);
        assert_eq!(tree(&fan), before);
        assert_eq!(
            fs::metadata(&fan).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    /// Something else at the record's name — a link to a record listing
    /// the store's files, say — proves nothing: nothing goes, and the reply
    /// names it.
    #[test]
    fn a_link_at_the_records_name_proves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let s = ThumbStore::new(&root);
        let key = s.put(b"own 6640").unwrap();
        let elsewhere = tmp.path().join("record-elsewhere");
        fs::rename(root.join(MADE_RECORD), &elsewhere).unwrap();
        symlink(&elsewhere, root.join(MADE_RECORD)).unwrap();

        let cleared = s.clear().unwrap();

        assert_eq!(cleared.removed, 0, "{cleared:?}");
        assert!(cleared
            .examples
            .iter()
            .any(|(p, _)| p == &root.join(MADE_RECORD)));
        assert_eq!(s.get(&key).unwrap(), b"own 6640");
        assert!(fs::symlink_metadata(root.join(MADE_RECORD))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
