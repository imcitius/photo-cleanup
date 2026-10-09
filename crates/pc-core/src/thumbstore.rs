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
    /// content key (32 lowercase hex digits), so [`ThumbStore::clear`]
    /// recognises the file as the store's own.
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

    /// Empty the cache, keeping the directory itself.
    ///
    /// Thumbnails are addressed by content, so nothing here is worth keeping
    /// once the rows that referenced them are gone — and a cache left behind
    /// after a reset is several gigabytes that no page will ever ask for.
    ///
    /// Only what the store itself writes goes (el-5x1uh C2): in a fan-out
    /// folder `ab/cd` (two lowercase hex digits each, a real folder of this
    /// user's, reached through the held descriptor of its parent, never a
    /// link) a plain file of this user's with a single name, called
    /// `abcd…` (32 hex digits) `.jpg`, or the store's own temporary
    /// `abcd….<pid>.<n>.tmp`. Each is removed with `unlinkat` relative to
    /// the open fan-out folder. Then the fan-out folders this left empty are
    /// removed (`rmdir` refuses one that is not). Anything else — another
    /// name, a link, a folder under a thumbnail's name, a second name of
    /// some other file, somebody else's file — stays where it is and is
    /// reported in [`Cleared::kept`].
    ///
    /// The cache folder is opened once and everything happens through that
    /// descriptor. In a bound data folder the binding confirms that this
    /// descriptor is the proven folder ([`crate::storage`]) before anything
    /// goes: a replacement put at its path — somebody else's pictures, say,
    /// before the check or right after it — is never listed, let alone
    /// emptied.
    ///
    /// What a check cannot see is a name swapped between the `fstatat` that
    /// proved an entry and the `unlinkat` that removes it. The folders are
    /// this user's and closed to everybody else (0700, see
    /// [`ThumbStore::put`]), so only a process of this user (or root) could
    /// do that, deliberately — outside the threat model ([`crate::storage`]).
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
    /// Thumbnails (and leftover temporary files of the store) removed.
    pub removed: u64,
    /// Entries left in place because they could not be proven to be the
    /// store's own, or could not be removed.
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
/// cache writable by everybody (el-5x1uh O3).
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

#[cfg(unix)]
mod platform {
    use super::{is_hex, is_key, Cleared, ThumbStore, DIR_MODE, FILE_MODE};
    use crate::anchored::Dir;
    use anyhow::{Context, Result};
    use std::io::{self, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
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

    /// Open the fan-out folder `name` in `parent` (making it with
    /// [`DIR_MODE`] first if `create`). It must be a real folder, not a
    /// link, and this user's. A folder of this user's that others could
    /// write to — made by an older version under a permissive umask — is
    /// narrowed to [`DIR_MODE`] through the open descriptor before anything
    /// is done in it.
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
        if md.mode() & 0o077 != 0 {
            dir.file()
                .set_permissions(std::fs::Permissions::from_mode(DIR_MODE))
                .map_err(|e| e.to_string())?;
        }
        Ok(dir)
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
        let first = fan_out(&root, a, true).map_err(|why| refused(&root.join(a), why))?;
        let fan = fan_out(&first, b, true).map_err(|why| refused(&first.join(b), why))?;
        write_then_rename(&fan, &format!("{key}.jpg"), key, jpeg)
    }

    /// Write beside the target and rename onto it, so a crash never leaves a
    /// half-written file that later looks valid.
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
    fn write_then_rename(fan: &Dir, name: &str, key: &str, jpeg: &[u8]) -> Result<()> {
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
        Ok(())
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
        let read = |dir: &Dir| {
            dir.names().with_context(|| {
                crate::tf!("не прочитать {0}", "cannot read {0}", dir.path().display())
            })
        };
        for first in read(&root)? {
            let Some(a) = fan_out_name(&first) else {
                out.keep(root.path().join(&first), not_ours());
                continue;
            };
            let dir_a = match fan_out(&root, a, false) {
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
                let dir_b = match fan_out(&dir_a, b, false) {
                    Ok(d) => d,
                    Err(why) => {
                        out.keep(dir_a.join(b), why);
                        continue;
                    }
                };
                let prefix = format!("{a}{b}");
                for name in read(&dir_b)? {
                    let path = dir_b.path().join(&name);
                    match name.to_str().filter(|n| is_own_file_name(n, &prefix)) {
                        None => out.keep(path, not_ours()),
                        Some(n) => match remove_own_file(&dir_b, n) {
                            Ok(()) => out.removed += 1,
                            Err(why) => out.keep(path, why),
                        },
                    }
                }
                remove_if_empty(&dir_a, b, &dir_b);
            }
            remove_if_empty(&root, a, &dir_a);
        }
        Ok(out)
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

    /// `<key>.jpg`, or the temporary `<key>.<pid>.<n>.tmp` of
    /// [`write_then_rename`], for a key in the fan-out folder `prefix`.
    fn is_own_file_name(name: &str, prefix: &str) -> bool {
        let Some((key, rest)) = name.split_at_checked(32) else {
            return false;
        };
        if !is_key(key) || !key.starts_with(prefix) {
            return false;
        }
        if rest == ".jpg" {
            return true;
        }
        let parts: Vec<&str> = rest.split('.').collect();
        matches!(parts.as_slice(), ["", pid, n, "tmp"]
            if !pid.is_empty() && !n.is_empty()
                && pid.bytes().all(|c| c.is_ascii_digit())
                && n.bytes().all(|c| c.is_ascii_digit()))
    }

    /// Remove `name` from the held fan-out folder if it is a plain file of
    /// this user's with a single name. A second name means the bytes are
    /// also somewhere else — not a file the store wrote — and stay.
    fn remove_own_file(dir: &Dir, name: &str) -> std::result::Result<(), String> {
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
        dir.remove_file_at(name).map_err(|e| e.to_string())
    }

    /// `rmdir` the fan-out folder `name` of `parent` if it is still the one
    /// held as `held`; the system refuses it if anything is left in it.
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
        // Only the planted entries and the fan-out folders holding them are left.
        assert_eq!(after.len(), planted.len() + 2, "{after:?}");
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
    /// thumbnails 0600, not 0777/0666; a fan-out folder of this user's that
    /// an older version left group/other-writable is tightened to 0700.
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
        for key in keys.lines() {
            let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
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
