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
//! (el-5x1uh O3); an existing fan-out folder must be a real folder of this
//! user's and is used as it is — never changed. A thumbnail is written to a
//! temporary file created exclusively beside its name and renamed there
//! without replacing anything: an entry that already bears the name is
//! never rewritten.
//!
//! Not covered here: an existing fan-out folder of this user's is not
//! proven to be one the store made. A folder put in its place by another
//! program of the same user is written into (follow-up el-u3x31).

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
    use super::{ThumbStore, DIR_MODE, FILE_MODE};
    use crate::anchored::Dir;
    use anyhow::{Context, Result};
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

    /// Open the fan-out folder `name` in `parent`, making it with
    /// [`DIR_MODE`] first if it is not there. It must be a real folder, not
    /// a link, and this user's. An existing folder is never changed.
    fn fan_out(parent: &Dir, name: &str, create: bool) -> std::result::Result<Dir, String> {
        if create {
            match parent.mkdir_mode(name, DIR_MODE) {
                Ok(()) => {}
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
        Ok(dir)
    }

    pub(super) fn read(store: &ThumbStore, key: &str) -> Option<Vec<u8>> {
        let root = Dir::open_following(&store.root).ok()?;
        let first = fan_out(&root, &key[0..2], false).ok()?;
        let fan = fan_out(&first, &key[2..4], false).ok()?;
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
        let fan = fan_out(&first, b, true).map_err(|why| refused(&first.join(b), why))?;
        let name = format!("{key}.jpg");
        match fan.entry_at(&name) {
            // Already stored: the key names the content (or, for a view, the
            // file it was rendered from), and an existing file is never
            // rewritten.
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
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
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
#[cfg(not(unix))]
mod platform {
    use super::ThumbStore;
    use anyhow::{Context, Result};
    use std::io::{self, Write};

    pub(super) fn read(store: &ThumbStore, key: &str) -> Option<Vec<u8>> {
        std::fs::read(store.path_for(key)).ok()
    }

    pub(super) fn write(store: &ThumbStore, key: &str, jpeg: &[u8]) -> Result<()> {
        store.check()?;
        let path = store.path_for(key);
        if path.exists() {
            return Ok(());
        }
        let fan = path.parent().context("no parent directory")?;
        std::fs::create_dir_all(fan)
            .with_context(|| crate::tf!("не создать {0}", "cannot create {0}", fan.display()))?;
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

    /// `umask` is process-wide, so the store runs under umask 000 in a child
    /// copy of the test binary. The cache folder and the fan-out folders it
    /// makes come out 0700 and thumbnails 0600, not 0777/0666. A fan-out
    /// folder that already exists — here one somebody left 0777 — is not
    /// the store's to change: it keeps its mode, and only what the store
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
        for key in text.lines() {
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
}
