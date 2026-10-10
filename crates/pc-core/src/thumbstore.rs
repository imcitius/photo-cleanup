//! Content-addressed thumbnail cache.
//!
//! Kept outside SQLite so it can be copied, rsynced or thrown away on its
//! own, and so identical pixels stored under many paths cost one file.
//!
//! # Generations: the cache never deletes (el-5x1uh, review el-19kbm)
//!
//! Thumbnails are written into one *generation*: a folder the store itself
//! creates inside the cache folder, exclusively (`mkdirat`, 0700, through
//! the held cache folder), named `generation-…`. Which generation is the
//! active one is recorded in the application's database by a
//! [`GenerationLedger`] — the folder's name and the identity it had when the
//! store made it (device, inode and, where the system keeps one, its birth
//! time) — never in a file inside the cache, which another program could
//! replace.
//!
//! A reset of the index ([`ThumbStore::reset`]) removes **nothing**. It
//! makes a new generation, records it, and from then on writes only there.
//! The previous generation, an older cache without generations, and anything
//! else found in the cache folder stay exactly where they are, and the reset
//! reports them with their approximate size as an old cache the user may
//! delete by hand to get the space back ([`Reset::kept`]). Nothing a name,
//! an owner or a record could vouch for is ever unlinked: there is no code
//! here that removes a file or a folder.
//!
//! When the store first needs its generation, a recorded one is used only
//! if the folder now bearing its name is the one recorded — a real folder,
//! not a link, this user's, closed to everybody else, with the recorded
//! identity. A missing, replaced or changed one is never adopted or touched:
//! a fresh generation is made instead, and the unknown folder is reported by
//! the next reset like any other old cache.
//!
//! Within the active generation, every entry is reached through held
//! folders without following a link, fan-out folders and files are created
//! exclusively, and an existing entry is never written to, appended to or
//! replaced.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Where the active generation of a cache is recorded: the application's
/// database (`pc_db::ThumbLedger`), keyed by the cache folder's path.
pub trait GenerationLedger: Send + Sync + std::fmt::Debug {
    /// The value last recorded for the cache at `root`, if any.
    fn recorded(&self, root: &Path) -> Result<Option<String>>;

    /// Record `value` for `root` if what is recorded there is still
    /// `previous`. `Ok(false)`: somebody recorded another one meanwhile, and
    /// nothing was changed.
    fn record(&self, root: &Path, previous: Option<&str>, value: &str) -> Result<bool>;
}

#[derive(Debug, Clone)]
pub struct ThumbStore {
    root: PathBuf,
    /// A bound data folder's proof ([`crate::storage`]), asked before every
    /// write. `None` for an ordinary folder.
    binding: Option<crate::storage::Binding>,
    /// Where the active generation is recorded. Without one (tests, a
    /// one-off store) the generation lives as long as this store and its
    /// clones.
    ledger: Option<Arc<dyn GenerationLedger>>,
    /// Shared by all clones, so a reset switches every job's store at once.
    active: Arc<Mutex<Active>>,
}

#[derive(Debug, Default)]
struct Active {
    /// The ledger has been read (once per store, at first use).
    looked: bool,
    /// The ledger's value as last read or written, for compare-and-set.
    recorded: Option<String>,
    /// The generation in use.
    generation: Option<Generation>,
}

/// A generation folder: its name in the cache folder and the identity it
/// had when the store created it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Generation {
    name: String,
    made: platform::Made,
}

impl Generation {
    fn encode(&self) -> String {
        format!("{} {}", self.name, self.made.encode())
    }

    fn parse(value: &str) -> Option<Self> {
        let (name, made) = value.split_once(' ')?;
        if !is_generation_name(name) {
            return None;
        }
        Some(Self {
            name: name.to_string(),
            made: platform::Made::parse(made)?,
        })
    }
}

/// Generation folders are named `generation-<unix seconds>-<pid>-<n>`.
pub const GENERATION_PREFIX: &str = "generation-";

fn is_generation_name(name: &str) -> bool {
    name.strip_prefix(GENERATION_PREFIX).is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit() || c == b'-')
    })
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
    /// A cache at `root` whose generation is not recorded anywhere: the
    /// first write makes one, used by this store and its clones only. The
    /// application records it with [`ThumbStore::with_ledger`].
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            binding: None,
            ledger: None,
            active: Default::default(),
        }
    }

    /// A cache in a bound data folder: nothing is written into `root`
    /// unless the binding confirms, right before, that it is still the
    /// proven folder. A replaced folder is left exactly as it is.
    pub fn bound(root: impl Into<PathBuf>, binding: crate::storage::Binding) -> Self {
        Self {
            binding: Some(binding),
            ..Self::new(root)
        }
    }

    /// Record the active generation in `ledger` (the application's
    /// database), so every start of the application finds the same one.
    pub fn with_ledger(self, ledger: Arc<dyn GenerationLedger>) -> Self {
        Self {
            ledger: Some(ledger),
            active: Default::default(),
            ..self
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

    /// The active generation's folder, once there is one.
    pub fn generation_dir(&self) -> Option<PathBuf> {
        let active = self.lock();
        active.generation.as_ref().map(|g| self.root.join(&g.name))
    }

    /// `<generation>/ab/cd/<key>.jpg` — two levels of fan-out keeps
    /// directories small enough that a filesystem listing stays usable.
    /// `None` before the store has a generation.
    pub fn path_for(&self, key: &str) -> Option<PathBuf> {
        let (a, b) = (key.get(0..2)?, key.get(2..4)?);
        Some(
            self.generation_dir()?
                .join(a)
                .join(b)
                .join(format!("{key}.jpg")),
        )
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

    /// The thumbnail stored under `key` in the active generation.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        if !is_key(key) {
            return None;
        }
        platform::read(self, key)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Start a new generation for a reset of the index. **Nothing is
    /// removed**: a new generation folder is created exclusively in the
    /// cache folder and recorded as the active one, and from now on every
    /// thumbnail is written there. The previous generation, a cache from a
    /// version without generations and everything else in the cache folder
    /// stay where they are; [`Reset::kept`] lists them with their size, for
    /// the user to delete by hand if they want the space.
    ///
    /// A bound cache is confirmed through the descriptor the new folder is
    /// made in ([`crate::storage`]): a replacement at the cache's path is
    /// never written into.
    pub fn reset(&self) -> Result<Reset> {
        let root = platform::open_root(self)?;
        let mut active = self.lock();
        let previous = match &self.ledger {
            Some(ledger) => ledger.recorded(&self.root)?,
            None => active.generation.as_ref().map(Generation::encode),
        };
        let (generation, _dir) = platform::make_generation(&root)?;
        if let Some(ledger) = &self.ledger {
            if !ledger.record(&self.root, previous.as_deref(), &generation.encode())? {
                anyhow::bail!(crate::tf!(
                    "кэш миниатюр {0}: активное поколение одновременно сменил кто-то ещё — ничего не удалено, повторите сброс",
                    "thumbnail cache {0}: somebody else switched its active generation at the same moment — nothing was removed; reset again",
                    self.root.display()
                ));
            }
        }
        active.looked = true;
        active.recorded = Some(generation.encode());
        let name = generation.name.clone();
        active.generation = Some(generation);
        drop(active);
        let mut out = Reset {
            generation: self.root.join(&name),
            ..Default::default()
        };
        platform::survey(&root, &name, &mut out);
        Ok(out)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Active> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Write `jpeg` as the thumbnail `key` (already validated), unless the
    /// active generation holds it already.
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

/// What [`ThumbStore::reset`] did: the new generation, and the old cache it
/// kept.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reset {
    /// The new, empty generation folder every thumbnail now goes to.
    pub generation: PathBuf,
    /// Entries in the cache folder other than the new generation — earlier
    /// generations, a cache without generations, anything else — none of
    /// them changed. The first [`KEPT_EXAMPLES`], largest first.
    pub kept: Vec<Kept>,
    /// How many entries were kept in all.
    pub kept_count: u64,
    /// Their approximate size on disk, in bytes.
    pub kept_bytes: u64,
    /// The size could not be measured in full (too many entries, or some
    /// could not be read): `kept_bytes` is a lower bound.
    pub kept_bytes_partial: bool,
}

/// One entry of the cache folder a reset kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    pub path: PathBuf,
    /// Approximate size on disk, everything in it included.
    pub bytes: u64,
}

/// How many kept entries a reset names; the count covers all of them.
pub const KEPT_EXAMPLES: usize = 20;

/// How many entries a reset measures in all before it stops counting and
/// reports a lower bound, so a reset of a huge cache returns in seconds.
const SURVEY_LIMIT: u64 = 1_000_000;

/// 32 lowercase hex digits: a key [`ThumbStore`] makes.
fn is_key(key: &str) -> bool {
    key.len() == 32
        && key
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// The approximate size of `path` and everything under it, links not
/// followed, counting at most `budget` entries (decremented).
fn measure(path: &Path, budget: &mut u64, partial: &mut bool) -> u64 {
    let mut total = 0u64;
    let mut todo = vec![path.to_path_buf()];
    while let Some(next) = todo.pop() {
        if *budget == 0 {
            *partial = true;
            break;
        }
        *budget -= 1;
        let Ok(md) = std::fs::symlink_metadata(&next) else {
            *partial = true;
            continue;
        };
        total += platform::on_disk(&md);
        if md.is_dir() {
            match std::fs::read_dir(&next) {
                Ok(entries) => todo.extend(entries.flatten().map(|e| e.path())),
                Err(_) => *partial = true,
            }
        }
    }
    total
}

/// Folders and thumbnails are made for this user only. The umask can only
/// take bits away from these, so a umask of 000 no longer leaves the cache
/// writable by everybody (el-5x1uh O3). Only what the store creates gets
/// these; an existing folder is never changed (B2).
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

#[cfg(unix)]
mod platform {
    use super::{measure, Generation, Kept, Reset, ThumbStore, DIR_MODE, FILE_MODE};
    use super::{GENERATION_PREFIX, KEPT_EXAMPLES, SURVEY_LIMIT};
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

    /// What identifies a generation folder the store created: device, inode
    /// and, where the system keeps one, the birth time (a folder's change
    /// time moves with every entry added, so it cannot be used).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Made {
        dev: u64,
        ino: u64,
        born: Option<(u64, u32)>,
    }

    impl Made {
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

        pub(super) fn encode(&self) -> String {
            match self.born {
                Some((s, ns)) => format!("{} {} {s} {ns}", self.dev, self.ino),
                None => format!("{} {} - -", self.dev, self.ino),
            }
        }

        pub(super) fn parse(text: &str) -> Option<Self> {
            let f: Vec<&str> = text.split(' ').collect();
            let [dev, ino, s, ns] = f.as_slice() else {
                return None;
            };
            let born = match (*s, *ns) {
                ("-", "-") => None,
                (s, ns) => Some((s.parse().ok()?, ns.parse().ok()?)),
            };
            Some(Self {
                dev: dev.parse().ok()?,
                ino: ino.parse().ok()?,
                born,
            })
        }
    }

    /// The cache folder, as the user configured it: a link at its own name
    /// is followed (the user may keep the cache elsewhere); everything below
    /// it is reached through descriptors and never through a link. An
    /// ordinary cache is made on first use; a bound one exists since the
    /// move and is never re-created by path. A bound one is confirmed
    /// through the descriptor everything is then done through.
    pub(super) fn open_root(store: &ThumbStore) -> Result<Dir> {
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

    /// The generation folder `g` in `root`, if what bears its name now is
    /// the folder the store made: a real folder (no link followed), with the
    /// recorded identity, this user's and closed to everybody else.
    fn open_generation(root: &Dir, g: &Generation) -> std::result::Result<Dir, String> {
        let dir = root.open_dir(&g.name).map_err(|e| match e.raw_os_error() {
            Some(libc::ELOOP) | Some(libc::ENOTDIR) => crate::tr!(
                "не каталог (ссылка или файл)",
                "not a folder (a link or a file)"
            )
            .to_string(),
            _ => e.to_string(),
        })?;
        let md = dir.file().metadata().map_err(|e| e.to_string())?;
        let now = Made::of(&md);
        let same = now.dev == g.made.dev
            && now.ino == g.made.ino
            && (g.made.born.is_none() || g.made.born == now.born);
        if !same {
            return Err(crate::tr!(
                "не та папка, которую создал кэш (идентичность не совпадает с записанной в базе)",
                "not the folder the cache created (its identity is not the one recorded in the database)"
            )
            .into());
        }
        if md.uid() != euid() || md.mode() & 0o077 != 0 {
            return Err(crate::tf!(
                "папка чужая или открыта для других (uid {0}, права {1:o})",
                "the folder is somebody else's or open to others (uid {0}, mode {1:o})",
                md.uid(),
                md.mode() & 0o777
            ));
        }
        Ok(dir)
    }

    /// Create a new generation folder in `root`, exclusively and with
    /// [`DIR_MODE`]. A name already taken is never adopted: the next one is
    /// tried.
    pub(super) fn make_generation(root: &Dir) -> Result<(Generation, Dir)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let name = format!(
                "{GENERATION_PREFIX}{}-{}-{}",
                crate::time::now_unix().max(0),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            match root.mkdir_mode(&name, DIR_MODE) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        crate::tf!(
                            "не создать {0}",
                            "cannot create {0}",
                            root.join(&name).display()
                        )
                    })
                }
            }
            let dir = root.open_dir(&name).with_context(|| {
                crate::tf!(
                    "не открыть {0}",
                    "cannot open {0}",
                    root.join(&name).display()
                )
            })?;
            let md = dir.file().metadata()?;
            if md.uid() != euid() || md.mode() & 0o077 != 0 {
                anyhow::bail!(crate::tf!(
                    "{0}: только что созданная папка поколения чужая или открыта для других — ничего не записано",
                    "{0}: the generation folder just made is somebody else's or open to others — nothing was written",
                    root.join(&name).display()
                ));
            }
            let made = Made::of(&md);
            return Ok((Generation { name, made }, dir));
        }
        anyhow::bail!(crate::tf!(
            "{0}: не удалось подобрать свободное имя для нового поколения кэша",
            "{0}: no free name for a new cache generation",
            root.path().display()
        ))
    }

    /// The active generation, held open. At the store's first use the
    /// ledger is read, and a recorded generation is used only if it is
    /// still the folder the store made; otherwise — and when nothing is
    /// recorded — a fresh one is made (`create`) and recorded, the unknown
    /// folder left as it is. Once in use, a generation that stops being
    /// that folder is a refusal: nothing is written until the next start.
    fn active(store: &ThumbStore, root: &Dir, create: bool) -> Result<Option<Dir>> {
        let mut active = store.lock();
        if !active.looked {
            if let Some(ledger) = &store.ledger {
                active.recorded = ledger.recorded(&store.root)?;
                if let Some(g) = active.recorded.as_deref().and_then(Generation::parse) {
                    if let Ok(dir) = open_generation(root, &g) {
                        active.generation = Some(g);
                        active.looked = true;
                        return Ok(Some(dir));
                    }
                }
            }
            active.looked = true;
        }
        if let Some(g) = &active.generation {
            return open_generation(root, g).map(Some).map_err(|why| {
                anyhow::anyhow!(crate::tf!(
                    "{0}: поколение кэша миниатюр заменено или изменено ({1}) — ничего не записано; при следующем запуске будет создано новое",
                    "{0}: the thumbnail cache's generation was replaced or changed ({1}) — nothing was written; a new one is made at the next start",
                    root.join(&g.name).display(),
                    why
                ))
            });
        }
        if !create {
            return Ok(None);
        }
        let (g, dir) = make_generation(root)?;
        if let Some(ledger) = &store.ledger {
            if !ledger.record(&store.root, active.recorded.as_deref(), &g.encode())? {
                // Another instance of the store recorded one meanwhile: use
                // that one if it is what it says. Ours stays, empty, and a
                // reset names it.
                let recorded = ledger.recorded(&store.root)?;
                let other = recorded.as_deref().and_then(Generation::parse);
                let opened = other
                    .as_ref()
                    .map(|o| open_generation(root, o))
                    .transpose()
                    .map_err(|why| anyhow::anyhow!(why))?;
                let (Some(other), Some(dir)) = (other, opened) else {
                    anyhow::bail!(crate::tr!(
                        "поколение кэша миниатюр одновременно сменил кто-то ещё — ничего не записано",
                        "somebody else switched the thumbnail cache's generation at the same moment — nothing was written"
                    ));
                };
                active.recorded = recorded;
                active.generation = Some(other);
                return Ok(Some(dir));
            }
            active.recorded = Some(g.encode());
        }
        active.generation = Some(g);
        Ok(Some(dir))
    }

    /// Open the fan-out folder `name` in `parent`, making it with
    /// [`DIR_MODE`] first if it is not there. It must be a real folder, not
    /// a link, and this user's. An existing folder is never changed (B2).
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
        let generation = active(store, &root, false).ok()??;
        let first = fan_out(&generation, &key[0..2], false).ok()?;
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
        let generation = active(store, &root, true)?.context(crate::tr!(
            "нет поколения кэша миниатюр",
            "no thumbnail cache generation"
        ))?;
        let (a, b) = (&key[0..2], &key[2..4]);
        let refused = |path: &Path, why: String| {
            anyhow::anyhow!(crate::tf!(
                "{0}: миниатюра не записана: {1}",
                "{0}: the thumbnail was not written: {1}",
                path.display(),
                why
            ))
        };
        let first =
            fan_out(&generation, a, true).map_err(|why| refused(&generation.join(a), why))?;
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

    /// Bytes on disk an entry takes.
    pub(super) fn on_disk(md: &std::fs::Metadata) -> u64 {
        md.blocks().saturating_mul(512)
    }

    /// Everything in the cache folder except the generation `active`, with
    /// its size; nothing is changed.
    pub(super) fn survey(root: &Dir, active: &str, out: &mut Reset) {
        let names = match root.names() {
            Ok(n) => n,
            Err(_) => {
                out.kept_bytes_partial = true;
                return;
            }
        };
        let mut budget = SURVEY_LIMIT;
        let mut kept: Vec<Kept> = Vec::new();
        for name in names {
            if name.to_str() == Some(active) {
                continue;
            }
            let path = root.path().join(&name);
            let bytes = measure(&path, &mut budget, &mut out.kept_bytes_partial);
            out.kept_count += 1;
            out.kept_bytes += bytes;
            kept.push(Kept { path, bytes });
        }
        kept.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        kept.truncate(KEPT_EXAMPLES);
        out.kept = kept;
    }
}

/// Platforms without descriptor-relative operations: folders and files are
/// created by path, exclusively (a generation folder with `create_dir`, a
/// thumbnail with `create_new`), so nothing that already bears a name is
/// ever written to; a generation is recognised by being a real folder (not
/// a link) under its recorded name.
#[cfg(not(unix))]
mod platform {
    use super::{measure, Generation, Kept, Reset, ThumbStore};
    use super::{GENERATION_PREFIX, KEPT_EXAMPLES, SURVEY_LIMIT};
    use anyhow::{Context, Result};
    use std::io::{self, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct Made;

    impl Made {
        pub(super) fn encode(&self) -> String {
            "-".into()
        }
        pub(super) fn parse(text: &str) -> Option<Self> {
            (text == "-").then_some(Made)
        }
    }

    /// The cache folder by path (there is no descriptor to hold).
    pub struct Dir(PathBuf);

    pub(super) fn open_root(store: &ThumbStore) -> Result<Dir> {
        store.check()?;
        if store.binding.is_none() {
            std::fs::create_dir_all(&store.root).with_context(|| {
                crate::tf!("не создать {0}", "cannot create {0}", store.root.display())
            })?;
        }
        Ok(Dir(store.root.clone()))
    }

    fn is_real_dir(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    }

    pub(super) fn make_generation(root: &Dir) -> Result<(Generation, ())> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..64 {
            let name = format!(
                "{GENERATION_PREFIX}{}-{}-{}",
                crate::time::now_unix().max(0),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            match std::fs::create_dir(root.0.join(&name)) {
                Ok(()) => return Ok((Generation { name, made: Made }, ())),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(e).with_context(|| {
                        crate::tf!(
                            "не создать {0}",
                            "cannot create {0}",
                            root.0.join(&name).display()
                        )
                    })
                }
            }
        }
        anyhow::bail!(crate::tf!(
            "{0}: не удалось подобрать свободное имя для нового поколения кэша",
            "{0}: no free name for a new cache generation",
            root.0.display()
        ))
    }

    fn active(store: &ThumbStore, root: &Dir, create: bool) -> Result<Option<PathBuf>> {
        let mut active = store.lock();
        if !active.looked {
            if let Some(ledger) = &store.ledger {
                active.recorded = ledger.recorded(&store.root)?;
                if let Some(g) = active.recorded.as_deref().and_then(Generation::parse) {
                    if is_real_dir(&root.0.join(&g.name)) {
                        active.generation = Some(g);
                    }
                }
            }
            active.looked = true;
        }
        if let Some(g) = &active.generation {
            let dir = root.0.join(&g.name);
            if !is_real_dir(&dir) {
                anyhow::bail!(crate::tf!(
                    "{0}: поколение кэша миниатюр заменено — ничего не записано",
                    "{0}: the thumbnail cache's generation was replaced — nothing was written",
                    dir.display()
                ));
            }
            return Ok(Some(dir));
        }
        if !create {
            return Ok(None);
        }
        let (g, ()) = make_generation(root)?;
        if let Some(ledger) = &store.ledger {
            if !ledger.record(&store.root, active.recorded.as_deref(), &g.encode())? {
                anyhow::bail!(crate::tr!(
                    "поколение кэша миниатюр одновременно сменил кто-то ещё — ничего не записано",
                    "somebody else switched the thumbnail cache's generation at the same moment — nothing was written"
                ));
            }
            active.recorded = Some(g.encode());
        }
        let dir = root.0.join(&g.name);
        active.generation = Some(g);
        Ok(Some(dir))
    }

    pub(super) fn read(store: &ThumbStore, key: &str) -> Option<Vec<u8>> {
        let root = Dir(store.root.clone());
        let dir = active(store, &root, false).ok()??;
        std::fs::read(
            dir.join(&key[0..2])
                .join(&key[2..4])
                .join(format!("{key}.jpg")),
        )
        .ok()
    }

    pub(super) fn write(store: &ThumbStore, key: &str, jpeg: &[u8]) -> Result<()> {
        let root = open_root(store)?;
        let dir = active(store, &root, true)?.context("no thumbnail cache generation")?;
        let fan = dir.join(&key[0..2]).join(&key[2..4]);
        std::fs::create_dir_all(&fan)
            .with_context(|| crate::tf!("не создать {0}", "cannot create {0}", fan.display()))?;
        let path = fan.join(format!("{key}.jpg"));
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

    pub(super) fn on_disk(md: &std::fs::Metadata) -> u64 {
        md.len()
    }

    pub(super) fn survey(root: &Dir, active: &str, out: &mut Reset) {
        let Ok(entries) = std::fs::read_dir(&root.0) else {
            out.kept_bytes_partial = true;
            return;
        };
        let mut budget = SURVEY_LIMIT;
        let mut kept: Vec<Kept> = Vec::new();
        for entry in entries.flatten() {
            if entry.file_name().to_str() == Some(active) {
                continue;
            }
            let path = entry.path();
            let bytes = measure(&path, &mut budget, &mut out.kept_bytes_partial);
            out.kept_count += 1;
            out.kept_bytes += bytes;
            kept.push(Kept { path, bytes });
        }
        kept.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        kept.truncate(KEPT_EXAMPLES);
        out.kept = kept;
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

    #[test]
    fn generation_names_are_recognised_exactly() {
        assert!(is_generation_name("generation-1791583092-4117-0"));
        assert!(!is_generation_name("generation-"));
        assert!(!is_generation_name("generation-1/../x"));
        assert!(!is_generation_name("ab"));
        let g = Generation::parse("generation-1-2-3 not a made").map(|g| g.name);
        assert_eq!(g, None);
    }
}

/// The generation model (el-5x1uh, review el-19kbm): a reset removes
/// nothing, the active generation comes from the ledger and is used only
/// while it is the folder the store made, writes never touch an existing
/// entry.
#[cfg(all(test, unix))]
mod generation_tests {
    use super::*;
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The database's part, in memory.
    #[derive(Debug, Default)]
    struct Ledger(Mutex<HashMap<PathBuf, String>>);

    impl GenerationLedger for Ledger {
        fn recorded(&self, root: &Path) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(root).cloned())
        }
        fn record(&self, root: &Path, previous: Option<&str>, value: &str) -> Result<bool> {
            let mut map = self.0.lock().unwrap();
            if map.get(root).map(String::as_str) != previous {
                return Ok(false);
            }
            map.insert(root.to_path_buf(), value.to_string());
            Ok(true)
        }
    }

    fn ledgered(root: &Path, ledger: &Arc<Ledger>) -> ThumbStore {
        ThumbStore::new(root).with_ledger(ledger.clone())
    }

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

    fn generations(root: &Path) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(GENERATION_PREFIX))
            .collect();
        out.sort();
        out
    }

    /// A reset changes nothing that exists: the store's own thumbnails,
    /// a cache of the old layout, a foreign file at the name an earlier
    /// version used for its record, a link to an outside photo and a
    /// foreign folder all keep every byte, mode and time. The reset names
    /// each of them with its size and from then on writes only into the
    /// new generation.
    #[test]
    fn a_reset_removes_nothing_and_names_everything_it_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let outside = tmp.path().join("outside-photo-4117.jpg");
        fs::write(&outside, b"outside photo 4117").unwrap();
        let ledger = Arc::new(Ledger::default());
        let s = ledgered(&root, &ledger);
        let keys: Vec<_> = (0..5)
            .map(|i| s.put(format!("own {i} 4118").as_bytes()).unwrap())
            .collect();
        let old = s.generation_dir().unwrap();
        fs::create_dir_all(root.join("ab/cd")).unwrap();
        fs::write(
            root.join("ab/cd")
                .join(format!("abcd{}.jpg", "6".repeat(28))),
            b"legacy thumbnail 4119",
        )
        .unwrap();
        fs::write(root.join(".thumbstore-made"), b"foreign notes 4120\n").unwrap();
        symlink(&outside, root.join("photo-link")).unwrap();
        fs::create_dir(root.join("album")).unwrap();
        fs::write(root.join("album/IMG_4121.JPG"), vec![9u8; 40_000]).unwrap();
        let before = tree(&root);
        let outside_before = fs::read(&outside).unwrap();

        let reset = s.reset().unwrap();

        // Everything that was there is there, unchanged; one new folder.
        let after = tree(&root);
        for entry in &before {
            assert!(after.contains(entry), "{entry:?} changed or went");
        }
        assert_eq!(after.len(), before.len() + 1, "{after:?}");
        assert_eq!(fs::read(&outside).unwrap(), outside_before);
        assert!(reset.generation.is_dir());
        assert_ne!(reset.generation, old);
        assert_eq!(s.generation_dir().unwrap(), reset.generation);
        // ab, .thumbstore-made, photo-link, album and the old generation.
        assert_eq!(reset.kept_count, 5, "{reset:?}");
        let named: Vec<_> = reset.kept.iter().map(|k| k.path.clone()).collect();
        for p in [
            old.clone(),
            root.join("ab"),
            root.join(".thumbstore-made"),
            root.join("photo-link"),
            root.join("album"),
        ] {
            assert!(named.contains(&p), "{p:?} not named: {reset:?}");
        }
        let album = reset
            .kept
            .iter()
            .find(|k| k.path == root.join("album"))
            .unwrap();
        assert!(album.bytes >= 40_000, "{album:?}");
        assert!(reset.kept_bytes >= 40_000 + 5 * 8, "{reset:?}");
        assert!(!reset.kept_bytes_partial);
        // The old thumbnails are no longer served; new ones go to the new
        // generation; the old generation stays as it was.
        assert!(keys.iter().all(|k| s.get(k).is_none()));
        let fresh = s.put(b"after reset 4122").unwrap();
        assert!(s.path_for(&fresh).unwrap().starts_with(&reset.generation));
        for entry in &before {
            assert!(tree(&root).contains(entry), "{entry:?} changed");
        }
        assert_eq!(
            ledger.recorded(&root).unwrap().unwrap().split(' ').next(),
            reset.generation.file_name().unwrap().to_str()
        );
    }

    /// The generation is recorded in the ledger, not in the cache: a store
    /// opened later on the same ledger reads the same thumbnails.
    #[test]
    fn a_later_store_on_the_same_ledger_uses_the_same_generation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let ledger = Arc::new(Ledger::default());
        let key = ledgered(&root, &ledger).put(b"own 5230").unwrap();
        let later = ledgered(&root, &ledger);
        assert_eq!(later.get(&key).unwrap(), b"own 5230");
        later.put(b"own 5231").unwrap();
        assert_eq!(generations(&root).len(), 1);
    }

    /// The recorded generation's name now bears something else — a
    /// replacement folder of this user's, 0700, holding a file at a
    /// thumbnail's name; a link to an outside folder; a plain file; the
    /// genuine folder opened to others. None of them is read from, written
    /// into or changed: a fresh generation is made and recorded.
    #[test]
    fn an_unexpected_active_generation_is_never_adopted_or_changed() {
        for kind in ["replaced", "link", "file", "opened"] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("thumbs");
            let outside = tmp.path().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("photo-6301.jpg"), b"outside 6301").unwrap();
            let ledger = Arc::new(Ledger::default());
            let first = ledgered(&root, &ledger);
            let key = first.put(b"own 6302").unwrap();
            let genuine = first.generation_dir().unwrap();
            let leaf = first.path_for(&key).unwrap();
            let rel = leaf.strip_prefix(&genuine).unwrap().to_path_buf();
            match kind {
                "replaced" => {
                    fs::rename(&genuine, tmp.path().join("saved")).unwrap();
                    fs::create_dir_all(genuine.join(rel.parent().unwrap())).unwrap();
                    fs::set_permissions(&genuine, fs::Permissions::from_mode(0o700)).unwrap();
                    fs::write(genuine.join(&rel), b"foreign 6303").unwrap();
                }
                "link" => {
                    fs::rename(&genuine, tmp.path().join("saved")).unwrap();
                    symlink(&outside, &genuine).unwrap();
                }
                "file" => {
                    fs::rename(&genuine, tmp.path().join("saved")).unwrap();
                    fs::write(&genuine, b"foreign 6304").unwrap();
                }
                _ => fs::set_permissions(&genuine, fs::Permissions::from_mode(0o755)).unwrap(),
            }
            let (before, outside_before) = (tree(&root), tree(&outside));

            let later = ledgered(&root, &ledger);
            assert!(later.get(&key).is_none(), "{kind}: read from it");
            let key2 = later.put(b"own 6305").unwrap();

            let fresh = later.generation_dir().unwrap();
            assert_ne!(fresh, genuine, "{kind}");
            assert!(later.path_for(&key2).unwrap().starts_with(&fresh));
            let after = tree(&root);
            for entry in &before {
                assert!(after.contains(entry), "{kind}: {entry:?} changed");
            }
            assert_eq!(tree(&outside), outside_before, "{kind}");
            let recorded = ledger.recorded(&root).unwrap().unwrap();
            assert!(recorded.starts_with(fresh.file_name().unwrap().to_str().unwrap()));
        }
    }

    /// Two stores made a generation each at the same moment (two processes
    /// on one database): the second one sees the first one's record and
    /// uses that generation; its own stays, empty, untouched.
    #[test]
    fn a_generation_recorded_by_another_store_meanwhile_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let ledger = Arc::new(Ledger::default());
        let a = ledgered(&root, &ledger);
        let b = ledgered(&root, &ledger);
        fs::create_dir(&root).unwrap();
        assert!(b.get("0123456789abcdef0123456789abcdef").is_none()); // b looked: nothing
        let key = a.put(b"own 7101").unwrap();
        b.put(b"own 7102").unwrap();
        assert_eq!(b.generation_dir(), a.generation_dir());
        assert_eq!(b.get(&key).unwrap(), b"own 7101");
        assert_eq!(generations(&root).len(), 2);
    }

    /// An existing entry at a thumbnail's name in the active generation is
    /// never rewritten, appended to or replaced: a foreign file keeps its
    /// bytes, a link is not followed and its target is unchanged.
    #[test]
    fn an_existing_entry_at_a_thumbnails_name_is_never_written() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("thumbs");
        let outside = tmp.path().join("outside-8401.jpg");
        fs::write(&outside, b"outside 8401").unwrap();
        let s = ThumbStore::new(&root);
        s.put(b"seed 8400").unwrap();
        let generation = s.generation_dir().unwrap();
        let plain = format!("84{}", "1".repeat(30));
        let linked = format!("84{}", "2".repeat(30));
        let fan = generation.join("84/11");
        fs::create_dir_all(&fan).unwrap();
        fs::write(fan.join(format!("{plain}.jpg")), b"foreign 8402").unwrap();
        let fan2 = generation.join("84/22");
        fs::create_dir_all(&fan2).unwrap();
        symlink(&outside, fan2.join(format!("{linked}.jpg"))).unwrap();
        let before = tree(&root);

        s.put_at(&plain, b"own 8403").unwrap();
        assert!(s.put_at(&linked, b"own 8404").is_err());

        assert_eq!(tree(&root), before);
        assert_eq!(fs::read(&outside).unwrap(), b"outside 8401");
        assert!(s.get(&linked).is_none(), "read through a link");
    }

    /// A fan-out name in the generation that is a link: nothing is written
    /// through it.
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
        let generation = s.generation_dir().unwrap();
        let _ = fs::create_dir(generation.join(&key[0..2]));
        let _ = fs::remove_dir(generation.join(&key[0..2]).join(&key[2..4]));
        symlink(&outside, generation.join(&key[0..2]).join(&key[2..4])).unwrap();
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
    /// new generation is made in the proven folder, moved aside; the
    /// replacement is not written into.
    #[test]
    fn a_reset_of_a_cache_replaced_right_after_the_check_writes_nothing_into_it() {
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
                    fs::create_dir_all(self.root.join("ab/cd")).unwrap();
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
        let binding = Arc::new(SwapAfterCheck {
            root: root.clone(),
            saved: tmp.path().join("saved-thumbs"),
            proven: ident(&root),
            swapped: AtomicBool::new(false),
        });
        let s = ThumbStore::bound(&root, binding.clone());
        let reset = s.reset().unwrap();
        assert!(binding.swapped.load(Ordering::SeqCst));
        let mut names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["ab", "foreign.txt"], "written into the replacement");
        assert_eq!(generations(&tmp.path().join("saved-thumbs")).len(), 1);
        let _ = reset;
    }

    /// `umask` is process-wide, so the store runs under umask 000 in a child
    /// copy of the test binary. The generation and fan-out folders come out
    /// 0700 and thumbnails 0600, not 0777/0666. A fan-out folder that
    /// already exists — here one somebody left 0777 — is not the store's to
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
            let legacy = s.generation_dir().unwrap().join("9e");
            fs::create_dir(&legacy).unwrap();
            fs::set_permissions(&legacy, fs::Permissions::from_mode(0o777)).unwrap();
            let key2 = format!("9e{}", "4".repeat(30));
            s.put_at(&key2, b"legacy 2208").unwrap();
            let generation = s.generation_dir().unwrap();
            fs::write(
                Path::new(&dir).join("keys"),
                format!("{}\n{key}\n{key2}", generation.display()),
            )
            .unwrap();
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "thumbstore::generation_tests::under_umask_000_the_cache_is_not_left_writable_by_others",
                "--exact",
                "--test-threads=1",
            ])
            .env(CHILD, tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let text = fs::read_to_string(tmp.path().join("keys")).unwrap();
        let mut lines = text.lines();
        let generation = PathBuf::from(lines.next().unwrap());
        let mode = |p: PathBuf| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(tmp.path().join("thumbs")), 0o700);
        assert_eq!(mode(generation.clone()), 0o700);
        for key in lines {
            let (a, b) = (&key[0..2], &key[2..4]);
            let made = if a == "9e" { 0o777 } else { 0o700 };
            assert_eq!(mode(generation.join(a)), made, "{a}");
            assert_eq!(mode(generation.join(a).join(b)), 0o700, "{a}/{b}");
            assert_eq!(
                mode(generation.join(a).join(b).join(format!("{key}.jpg"))),
                0o600,
                "{key}"
            );
        }
    }
}
