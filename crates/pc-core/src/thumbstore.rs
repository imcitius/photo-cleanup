//! Content-addressed thumbnail cache.
//!
//! Kept outside SQLite so it can be copied, rsynced or thrown away on its
//! own, and so identical pixels stored under many paths cost one file.
//!
//! # The cache never removes anything (el-5x1uh, user decision 2026-10-10)
//!
//! There is no code here that deletes, renames away, truncates, appends to
//! or changes the permissions of anything in the cache. A reset of the
//! index does not touch the cache at all: thumbnails are addressed by their
//! content (a rendered view by the file and its size and time), so the ones
//! already stored stay valid and are found again when the archive is
//! indexed anew. Thumbnails nothing refers to any more keep their disk
//! space until the user deletes the cache folder by hand (DESKTOP.md).
//!
//! # Writing
//!
//! A thumbnail lives at `ab/cd/<key>.jpg` in the cache folder. On Unix the
//! cache folder is opened once (a link at its own name is followed — the
//! user may keep the cache elsewhere) and confirmed through that descriptor
//! for a bound data folder ([`crate::storage`]); everything below it is
//! reached through held folders without following a link. A fan-out folder
//! the store makes is made 0700, a thumbnail 0600, whatever the umask
//! (el-5x1uh O3). A thumbnail is written to a temporary file created
//! exclusively beside its name and renamed there without replacing
//! anything: an entry that already bears the name is never rewritten.
//!
//! # Only into fan-out folders this process made (el-u3x31)
//!
//! A new thumbnail is written only into fan-out folders the store made in
//! this process: right after `mkdirat` the folder is opened without
//! following a link and its device, inode and birth time are kept in
//! memory; an existing folder is written into only if it is that very
//! folder. Nothing records the origin across runs (no provenance file, no
//! database record — removed by user decision, el-5x1uh), so a folder from
//! an earlier run cannot be told from one another program of the same user
//! put at its name. Such a folder is never changed or written into, and
//! nothing is made inside it: the thumbnail is refused with the folder's
//! path and the reason (the index job lists it among its refusals). A
//! thumbnail already stored under its key is still found and used — that
//! writes nothing. Not covered: a folder swapped between `mkdirat` and the
//! open right after it (a deliberate syscall-window race).

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct ThumbStore {
    root: PathBuf,
    /// A bound data folder's proof ([`crate::storage`]), asked before every
    /// write. `None` for an ordinary folder.
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

    /// A cache in a bound data folder: nothing is written into `root`
    /// unless the binding confirms, right before, that it is still the
    /// proven folder. A replaced folder is left exactly as it is.
    pub fn bound(root: impl Into<PathBuf>, binding: crate::storage::Binding) -> Self {
        Self {
            root: root.into(),
            binding: Some(binding),
        }
    }

    /// For a bound cache: it is still the proven folder, so a change made
    /// now would land in it ([`crate::storage`]). Always `Ok` otherwise.
    /// Every write asks this itself; callers that change other things along
    /// with the cache ask first, so a refusal changes nothing.
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
        self.write(&key, jpeg)?;
        Ok(key)
    }

    /// Store under a key of the caller's choosing, for things that are not
    /// identified by their own content — a rendered view of a file, which is
    /// keyed by the file it was rendered from. The key has the shape of a
    /// content key (32 lowercase hex digits). A thumbnail already stored
    /// under the key is kept as it is, never rewritten.
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

    /// The thumbnail stored under `key`.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        if !is_key(key) {
            return None;
        }
        platform::read(self, key)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Write `jpeg` as the thumbnail `key` (already validated), unless the
    /// cache holds it already.
    fn write(&self, key: &str, jpeg: &[u8]) -> Result<()> {
        // One writer per key at a time in this process: the second one
        // finds the first one's file instead of making a temporary of its
        // own that could not be published.
        static STRIPES: [Mutex<()>; 16] = [const { Mutex::new(()) }; 16];
        let stripe = usize::from(key.as_bytes()[0]) % STRIPES.len();
        let _guard = STRIPES[stripe].lock().unwrap_or_else(|e| e.into_inner());
        platform::write(self, key, jpeg)
    }
}

/// 32 lowercase hex digits: a key [`ThumbStore`] makes.
fn is_key(key: &str) -> bool {
    key.len() == 32
        && key
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Why nothing is written into an existing fan-out folder the store cannot
/// prove it made (el-u3x31).
fn unproven() -> String {
    crate::tr!(
        "папка уже была и не создана кэшем миниатюр в этом запуске — в неё ничего не записано, она оставлена как есть",
        "the folder was already there and is not one the thumbnail cache made in this run — nothing is written into it, it is left as it is"
    )
    .to_string()
}

/// Folders and thumbnails are made for this user only. The umask can only
/// take bits away from these, so a umask of 000 no longer leaves the cache
/// writable by everybody (el-5x1uh O3). Only what the store creates gets
/// these; an existing folder is never changed.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

#[cfg(unix)]
mod platform {
    use super::{unproven, ThumbStore, DIR_MODE, FILE_MODE};
    use crate::anchored::Dir;
    use anyhow::{Context, Result};
    use std::collections::HashSet;
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    fn euid() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::geteuid() }
    }

    /// The cache folder, as the user configured it: a link at its own name
    /// is followed (the user may keep the cache elsewhere); everything below
    /// it is reached through descriptors and never through a link. An
    /// ordinary cache is made on first use; a bound one exists since the
    /// move and is never re-created by path. A bound one is confirmed
    /// through the descriptor everything is then done through.
    fn open_root(store: &ThumbStore) -> Result<Dir> {
        let root = match Dir::open_following(&store.root) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound && store.binding.is_none() => {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(DIR_MODE)
                    .create(&store.root)
                    .with_context(|| {
                        crate::tf!("не создать {0}", "cannot create {0}", store.root.display())
                    })?;
                Dir::open_following(&store.root).with_context(|| {
                    crate::tf!("не открыть {0}", "cannot open {0}", store.root.display())
                })?
            }
            Err(e) => {
                store.check()?;
                return Err(e).with_context(|| {
                    crate::tf!("не открыть {0}", "cannot open {0}", store.root.display())
                });
            }
        };
        if let Some(b) = &store.binding {
            b.check_thumbnail_folder(root.file())
                .map_err(crate::storage::NotBound)?;
        }
        Ok(root)
    }

    /// Identity of a folder the store made: device, inode and, where the
    /// filesystem keeps one, birth time (so an inode number reused after the
    /// store's folder was deleted does not pass for it).
    type Made = (u64, u64, Option<std::time::SystemTime>);

    /// Every fan-out folder this process made, by identity captured through
    /// the descriptor right after `mkdirat` (el-u3x31). It lives only as long
    /// as the process: there is no provenance file and no database record
    /// (removed by user decision, el-5x1uh), so a folder made by an earlier
    /// run cannot be told from one another program put at its name.
    static MADE: Mutex<Option<HashSet<Made>>> = Mutex::new(None);

    fn made(md: &std::fs::Metadata) -> Made {
        (md.dev(), md.ino(), md.created().ok())
    }

    /// A fan-out folder, opened, and whether this process made it.
    struct Fan {
        dir: Dir,
        made: bool,
    }

    /// Open the fan-out folder `name` in `parent`. With `create` it is made
    /// first with [`DIR_MODE`] if it is not there, and its identity recorded
    /// in [`MADE`]. It must be a real folder, not a link, and this user's. An
    /// existing folder is never changed; whether it may be written into is
    /// [`Fan::made`].
    fn fan_out(parent: &Dir, name: &str, create: bool) -> std::result::Result<Fan, String> {
        let mut fresh = false;
        if create {
            match parent.mkdir_mode(name, DIR_MODE) {
                Ok(()) => fresh = true,
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
        let mut set = MADE.lock().unwrap_or_else(|e| e.into_inner());
        let set = set.get_or_insert_with(HashSet::new);
        let made = if fresh {
            set.insert(made(&md));
            true
        } else {
            set.contains(&made(&md))
        };
        Ok(Fan { dir, made })
    }

    pub(super) fn read(store: &ThumbStore, key: &str) -> Option<Vec<u8>> {
        let root = Dir::open_following(&store.root).ok()?;
        let first = fan_out(&root, &key[0..2], false).ok()?.dir;
        let fan = fan_out(&first, &key[2..4], false).ok()?.dir;
        let mut file = fan.open_file(&format!("{key}.jpg"), false).ok()?;
        if !file.metadata().ok()?.is_file() {
            return None;
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).ok()?;
        Some(bytes)
    }

    /// Set once a volume answered that it cannot rename without replacing:
    /// from then on thumbnails are created at their name directly.
    static NO_EXCLUSIVE_RENAME: AtomicBool = AtomicBool::new(false);

    pub(super) fn write(store: &ThumbStore, key: &str, jpeg: &[u8]) -> Result<()> {
        let root = open_root(store)?;
        let (a, b) = (&key[0..2], &key[2..4]);
        let refused = |path: &Path, why: String| {
            anyhow::anyhow!(crate::tf!(
                "{0}: миниатюра не записана: {1}",
                "{0}: the thumbnail was not written: {1}",
                path.display(),
                why
            ))
        };
        let first = fan_out(&root, a, true).map_err(|why| refused(&root.join(a), why))?;
        // Nothing is made inside a folder the store cannot prove it made.
        let fan = match fan_out(&first.dir, b, first.made) {
            Ok(fan) => fan,
            Err(_) if !first.made && first.dir.entry_at(b).is_err() => {
                return Err(refused(&root.join(a), unproven()))
            }
            Err(why) => return Err(refused(&first.dir.join(b), why)),
        };
        // The folder a new thumbnail would go into, if the store cannot
        // prove it made it (el-u3x31).
        let foreign = if !first.made {
            Some(root.join(a))
        } else if !fan.made {
            Some(fan.dir.path().to_path_buf())
        } else {
            None
        };
        let fan = fan.dir;
        let name = format!("{key}.jpg");
        match fan.entry_at(&name) {
            // Already stored: the key names the content (or, for a view, the
            // file it was rendered from), and an existing file is never
            // rewritten. Using it writes nothing, so this holds in a folder
            // of unknown origin too.
            Ok(entry) if entry.is_file() => return Ok(()),
            Ok(_) => {
                return Err(refused(
                    &fan.join(&name),
                    crate::tr!(
                        "под этим именем не обычный файл — оставлен как есть",
                        "something other than a plain file bears the name — left as it is"
                    )
                    .into(),
                ))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if let Some(folder) = foreign {
                    return Err(refused(&folder, unproven()));
                }
            }
            Err(e) => return Err(refused(&fan.join(&name), e.to_string())),
        }
        publish(&fan, &name, key, jpeg)
    }

    /// Write the thumbnail beside its name and rename it there without
    /// replacing anything, so a crash never leaves a half-written file under
    /// a valid name and nothing that bears the name is ever overwritten.
    ///
    /// The temporary file is created new (`O_EXCL|O_NOFOLLOW`,
    /// [`FILE_MODE`]) under a name unique per writer. One that cannot be
    /// published is left where it is and named in the error — the store
    /// never removes anything.
    fn publish(fan: &Dir, name: &str, key: &str, jpeg: &[u8]) -> Result<()> {
        let cannot = |e: io::Error, what: &str| {
            anyhow::Error::new(e).context(crate::tf!(
                "не записать {0}",
                "cannot write {0}",
                fan.join(what).display()
            ))
        };
        if !NO_EXCLUSIVE_RENAME.load(Ordering::Relaxed) {
            static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let tmp = format!("{key}.{}.{sequence}.tmp", std::process::id());
            let mut file = fan
                .create_new(&tmp, FILE_MODE)
                .map_err(|e| cannot(e, &tmp))?;
            let left = |e: io::Error| {
                cannot(e, name).context(crate::tf!(
                    "временный файл {0} оставлен на месте",
                    "the temporary file {0} is left in place",
                    fan.join(&tmp).display()
                ))
            };
            file.write_all(jpeg).map_err(left)?;
            drop(file);
            match fan.rename_no_replace(&tmp, name) {
                Ok(()) => return Ok(()),
                // Another process published the same key first; its file
                // stays, and so does this temporary.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
                Err(e) if crate::disk::lacks_exclusive_rename(&e) => {
                    NO_EXCLUSIVE_RENAME.store(true, Ordering::Relaxed);
                }
                Err(e) => return Err(left(e)),
            }
        }
        // The volume cannot rename without replacing: create the thumbnail
        // at its name, exclusively. A crash in the middle leaves a short
        // file there; nothing else is ever overwritten.
        let mut file = match fan.create_new(name, FILE_MODE) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => return Err(cannot(e, name)),
        };
        file.write_all(jpeg).map_err(|e| cannot(e, name))
    }
}

/// Platforms without descriptor-relative operations: fan-out folders are
/// made by path and a thumbnail is created at its name exclusively
/// (`create_new`), so nothing that already bears a name is ever written to.
/// A crash in the middle of a write can leave a short file there.
///
/// A fan-out folder is written into only if this process made it
/// (el-u3x31): its path and creation time, read right after `create_dir`,
/// are kept in memory and must match. A volume that reports no creation
/// time proves nothing, so on it only — nothing is written into an existing
/// folder at all. Everything here goes by path: a folder swapped between the
/// check and the write is not detected (no descriptor-relative calls).
#[cfg(not(unix))]
mod platform {
    use super::ThumbStore;
    use anyhow::{Context, Result};
    use std::collections::HashSet;
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::SystemTime;

    static MADE: Mutex<Option<HashSet<(PathBuf, SystemTime)>>> = Mutex::new(None);

    /// The fan-out folder at `path`, made first if `create` and it is not
    /// there: `Ok(true)` if this process made it, `Ok(false)` if it exists
    /// but its origin is unknown.
    fn fan_out(path: &Path, create: bool) -> io::Result<bool> {
        if create {
            match std::fs::create_dir(path) {
                Ok(()) => {
                    if let Ok(made) = std::fs::symlink_metadata(path)?.created() {
                        let mut set = MADE.lock().unwrap_or_else(|e| e.into_inner());
                        set.get_or_insert_with(HashSet::new)
                            .insert((path.to_path_buf(), made));
                    }
                    return Ok(true);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        let md = std::fs::symlink_metadata(path)?;
        if !md.is_dir() {
            return Err(io::Error::other(crate::tr!(
                "не каталог (ссылка или файл)",
                "not a folder (a link or a file)"
            )));
        }
        let Ok(created) = md.created() else {
            return Ok(false);
        };
        let set = MADE.lock().unwrap_or_else(|e| e.into_inner());
        Ok(set
            .as_ref()
            .is_some_and(|set| set.contains(&(path.to_path_buf(), created))))
    }

    pub(super) fn read(store: &ThumbStore, key: &str) -> Option<Vec<u8>> {
        std::fs::read(store.path_for(key)).ok()
    }

    pub(super) fn write(store: &ThumbStore, key: &str, jpeg: &[u8]) -> Result<()> {
        store.check()?;
        let path = store.path_for(key);
        let refused = |at: &Path, why: String| {
            anyhow::anyhow!(crate::tf!(
                "{0}: миниатюра не записана: {1}",
                "{0}: the thumbnail was not written: {1}",
                at.display(),
                why
            ))
        };
        std::fs::create_dir_all(store.root()).with_context(|| {
            crate::tf!(
                "не создать {0}",
                "cannot create {0}",
                store.root().display()
            )
        })?;
        let fan = path.parent().context("no parent directory")?;
        let first = fan.parent().context("no parent directory")?;
        let first_made = fan_out(first, true).map_err(|e| refused(first, e.to_string()))?;
        let fan_made = match fan_out(fan, first_made) {
            Ok(made) => made,
            Err(_) if !first_made && !fan.exists() => {
                return Err(refused(first, super::unproven()))
            }
            Err(e) => return Err(refused(fan, e.to_string())),
        };
        if path.exists() {
            return Ok(());
        }
        if !first_made {
            return Err(refused(first, super::unproven()));
        }
        if !fan_made {
            return Err(refused(fan, super::unproven()));
        }
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(e) => {
                return Err(e).with_context(|| {
                    crate::tf!("не записать {0}", "cannot write {0}", path.display())
                })
            }
        };
        file.write_all(jpeg)
            .with_context(|| crate::tf!("не записать {0}", "cannot write {0}", path.display()))
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
    use super::*;

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
    fn a_missing_or_malformed_key_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let s = ThumbStore::new(tmp.path());
        assert!(s.get("deadbeefdeadbeef").is_none());
        assert!(s.get("x").is_none());
        assert!(s.get("0123456789abcdef0123456789abcdef").is_none());
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

/// Writing on Unix: nothing existing is written to, changed or followed,
/// and what the store creates is this user's only (el-5x1uh O3).
#[cfg(all(test, unix))]
mod unix_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// Kind, mode, owner, size, modification and change time and bytes of
    /// everything under `dir`, links not followed.
    fn tree(dir: &Path) -> Vec<(String, String)> {
        let mut out: Vec<_> = walkdir::WalkDir::new(dir)
            .min_depth(1)
            .into_iter()
            .map(|e| {
                let e = e.unwrap();
                let p = e.path();
                let m = fs::symlink_metadata(p).unwrap();
                let rel = p.strip_prefix(dir).unwrap().display().to_string();
                let what = if m.file_type().is_symlink() {
                    format!("link -> {}", fs::read_link(p).unwrap().display())
                } else if m.is_dir() {
                    "dir".into()
                } else {
                    format!("{:?}", fs::read(p).unwrap())
                };
                let sig = format!(
                    "{what} {:o} {} {} {}.{} {}.{} {}",
                    m.mode(),
                    m.uid(),
                    m.len(),
                    m.mtime(),
                    m.mtime_nsec(),
                    m.ctime(),
                    m.ctime_nsec(),
                    m.ino()
                );
                (rel, sig)
            })
            .collect();
        out.sort();
        out
    }

    /// A cache an earlier version wrote — fan-out folders 0755, thumbnails
    /// 0644, the same `ab/cd/<key>.jpg` layout — is read as it is, and
    /// storing a thumbnail it already holds changes nothing in it.
    #[test]
    fn a_cache_written_by_an_earlier_version_is_read_and_left_as_it_is() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let jpeg = b"earlier version 5170";
        let key = hex32(&blake3_of(jpeg)[..16]);
        let path = root
            .join(&key[0..2])
            .join(&key[2..4])
            .join(format!("{key}.jpg"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for dir in [
            &root.join(&key[0..2]),
            &path.parent().unwrap().to_path_buf(),
        ] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(&path, jpeg).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let before = tree(&root);

        let s = ThumbStore::new(&root);
        assert_eq!(s.path_for(&key), path);
        assert_eq!(s.get(&key).unwrap(), jpeg);
        assert_eq!(s.put(jpeg).unwrap(), key);
        s.put_at(&key, b"another rendering 5171").unwrap();

        assert_eq!(tree(&root), before);
        assert_eq!(s.get(&key).unwrap(), jpeg);
    }

    /// An existing entry at a thumbnail's name is never rewritten, appended
    /// to or replaced: a foreign file keeps its bytes, a link is not
    /// followed and its target is unchanged.
    #[test]
    fn an_existing_entry_at_a_thumbnails_name_is_never_written() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let outside = tmp.path().join("outside-8401.jpg");
        fs::write(&outside, b"outside 8401").unwrap();
        let s = ThumbStore::new(&root);
        s.put(b"seed 8400").unwrap();
        let plain = format!("84{}", "1".repeat(30));
        let linked = format!("84{}", "2".repeat(30));
        let fan = root.join("84/11");
        fs::create_dir_all(&fan).unwrap();
        fs::write(fan.join(format!("{plain}.jpg")), b"foreign 8402").unwrap();
        let fan2 = root.join("84/22");
        fs::create_dir_all(&fan2).unwrap();
        symlink(&outside, fan2.join(format!("{linked}.jpg"))).unwrap();
        let before = tree(&root);

        s.put_at(&plain, b"own 8403").unwrap();
        assert!(s.put_at(&linked, b"own 8404").is_err());

        assert_eq!(tree(&root), before);
        assert_eq!(fs::read(&outside).unwrap(), b"outside 8401");
        assert!(s.get(&linked).is_none(), "read through a link");
    }

    /// A fan-out name that is a link: nothing is written through it.
    #[test]
    fn a_thumbnail_is_not_written_through_a_linked_fan_out_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let s = ThumbStore::new(&root);
        s.put(b"seed 7780").unwrap();
        let jpeg = b"linked fan-out 7781";
        let key = hex32(&blake3_of(jpeg)[..16]);
        let _ = fs::create_dir(root.join(&key[0..2]));
        let _ = fs::remove_dir(root.join(&key[0..2]).join(&key[2..4]));
        symlink(&outside, root.join(&key[0..2]).join(&key[2..4])).unwrap();
        assert!(s.put(jpeg).is_err());
        assert!(s.put_at(&key, jpeg).is_err());
        assert_eq!(
            fs::read_dir(&outside).unwrap().count(),
            0,
            "written through the link"
        );
    }

    /// The cache's path is replaced right after the binding confirmed it
    /// (by the binding itself, the narrowest place a test can reach). The
    /// thumbnail goes into the proven folder, moved aside; the replacement
    /// is not written into.
    #[test]
    fn a_cache_replaced_right_after_the_check_is_not_written_into() {
        #[derive(Debug)]
        struct SwapAfterCheck {
            root: PathBuf,
            saved: PathBuf,
            proven: (u64, u64),
            swapped: AtomicBool,
        }
        fn ident(p: &Path) -> (u64, u64) {
            let m = fs::symlink_metadata(p).unwrap();
            (m.dev(), m.ino())
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
                    fs::create_dir_all(&self.root).unwrap();
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
        fs::create_dir(&root).unwrap();
        let saved = tmp.path().join("saved-thumbs");
        let binding = Arc::new(SwapAfterCheck {
            root: root.clone(),
            saved: saved.clone(),
            proven: ident(&root),
            swapped: AtomicBool::new(false),
        });
        let s = ThumbStore::bound(&root, binding.clone());
        let key = s.put(b"swapped 6231").unwrap();
        assert!(binding.swapped.load(Ordering::SeqCst));
        let names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["foreign.txt"], "written into the replacement");
        let stored = saved
            .join(&key[0..2])
            .join(&key[2..4])
            .join(format!("{key}.jpg"));
        assert_eq!(fs::read(stored).unwrap(), b"swapped 6231");
    }

    /// el-u3x31: a leaf fan-out folder the store made is moved aside and a
    /// folder of somebody's — 0750, with a note in it — is put at its name.
    /// A new thumbnail for that folder is refused with the reason, and
    /// neither the foreign folder nor the store's own, moved aside, is
    /// changed. Moved back, the store's own folder is written into again.
    #[test]
    fn a_substituted_leaf_fan_out_folder_is_not_written_into() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let s = ThumbStore::new(&root);
        let seed = s.put(b"seed 3310").unwrap();
        let (a, b) = (&seed[0..2], &seed[2..4]);
        let fan = root.join(a).join(b);
        let aside = tmp.path().join("aside-3310");
        fs::rename(&fan, &aside).unwrap();
        fs::create_dir(&fan).unwrap();
        fs::set_permissions(&fan, fs::Permissions::from_mode(0o750)).unwrap();
        fs::write(fan.join("foreign-notes"), b"foreign 3311").unwrap();
        let (foreign, own) = (tree(&fan), tree(&aside));

        let key = format!("{a}{b}{}", "3".repeat(28));
        let refused = s.put_at(&key, b"new 3312").unwrap_err().to_string();
        assert!(
            refused.contains("not one the thumbnail cache made"),
            "{refused}"
        );
        assert!(refused.contains(&fan.display().to_string()), "{refused}");
        assert_eq!(tree(&fan), foreign);
        assert_eq!(tree(&aside), own);
        assert!(s.get(&key).is_none());

        fs::rename(&fan, tmp.path().join("foreign-3310")).unwrap();
        fs::rename(&aside, &fan).unwrap();
        s.put_at(&key, b"new 3312").unwrap();
        assert_eq!(s.get(&key).unwrap(), b"new 3312");
        assert_eq!(s.get(&seed).unwrap(), b"seed 3310");
    }

    /// The same at the first level: nothing is made inside a substituted
    /// `ab` folder, not even the `cd` folder below it.
    #[test]
    fn nothing_is_made_inside_a_substituted_first_level_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let s = ThumbStore::new(&root);
        let seed = s.put(b"seed 4410").unwrap();
        let first = root.join(&seed[0..2]);
        fs::rename(&first, tmp.path().join("aside-4410")).unwrap();
        fs::create_dir(&first).unwrap();
        fs::write(first.join("foreign-notes"), b"foreign 4411").unwrap();
        let before = tree(&first);

        let key = format!("{}ff{}", &seed[0..2], "4".repeat(28));
        let refused = s.put_at(&key, b"new 4412").unwrap_err().to_string();
        assert!(
            refused.contains("not one the thumbnail cache made"),
            "{refused}"
        );
        assert!(refused.contains(&first.display().to_string()), "{refused}");
        assert_eq!(tree(&first), before);
    }

    /// Folders made by an earlier run cannot be told from folders another
    /// program put there (no provenance record, el-5x1uh), so a new
    /// thumbnail is not written into them; a thumbnail already stored there
    /// is still found and used, which writes nothing.
    #[test]
    fn a_folder_from_an_earlier_run_is_read_but_not_written_into() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let jpeg = b"earlier run 5520";
        let key = hex32(&blake3_of(jpeg)[..16]);
        let fan = root.join(&key[0..2]).join(&key[2..4]);
        fs::create_dir_all(&fan).unwrap();
        for dir in [&root.join(&key[0..2]), &fan] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(fan.join(format!("{key}.jpg")), jpeg).unwrap();
        let before = tree(&root);

        let s = ThumbStore::new(&root);
        assert_eq!(s.put(jpeg).unwrap(), key);
        let other = format!("{}{}", &key[0..4], "5".repeat(28));
        assert!(s.put_at(&other, b"new 5521").is_err());
        assert_eq!(tree(&root), before);
        assert_eq!(s.get(&key).unwrap(), jpeg);
    }

    /// `umask` is process-wide, so the store runs under umask 000 in a child
    /// copy of the test binary. The cache folder and the fan-out folders it
    /// makes come out 0700 and thumbnails 0600, not 0777/0666. A fan-out
    /// folder that already exists — here one somebody left 0777 — is not
    /// the store's to change or to write into (el-u3x31): it keeps its mode
    /// and stays empty, and the thumbnail that would have gone there is
    /// refused.
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
            let refused = s.put_at(&key2, b"legacy 2208").unwrap_err().to_string();
            assert!(
                refused.contains("not one the thumbnail cache made"),
                "{refused}"
            );
            fs::write(Path::new(&dir).join("keys"), &key).unwrap();
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "thumbstore::unix_tests::under_umask_000_the_cache_is_not_left_writable_by_others",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD, tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let text = fs::read_to_string(tmp.path().join("keys")).unwrap();
        let root = tmp.path().join("thumbs");
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(root.clone()), 0o700);
        assert_eq!(mode(root.join("9e")), 0o777);
        assert_eq!(fs::read_dir(root.join("9e")).unwrap().count(), 0);
        for key in text.lines() {
            let (a, b) = (&key[0..2], &key[2..4]);
            assert_eq!(mode(root.join(a)), 0o700, "{a}");
            assert_eq!(mode(root.join(a).join(b)), 0o700, "{a}/{b}");
            assert_eq!(
                mode(root.join(a).join(b).join(format!("{key}.jpg"))),
                0o600,
                "{key}"
            );
        }
    }
}
