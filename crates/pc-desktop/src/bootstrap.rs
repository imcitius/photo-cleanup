//! The bootstrap file: which data directory the user chose.
//!
//! ```json
//! { "version": 1, "mode": "custom", "data_dir": "D:\\Фото архив\\pc",
//!   "previous": { "mode": "system", "data_dir": null } }
//! ```
//!
//! `previous` is the choice before the last change, kept until the new one
//! has survived a launch, so a failed move can be undone from the error
//! screen. Portable mode is never written here: it is asked for by a marker
//! beside the executable, and a portable copy must not leave traces in the
//! user profile.
//!
//! A choice made by moving the data ([`crate::relocate`]) is *bound*: next
//! to the path it records which objects the proven copy was — the volume,
//! the folder, the database file and the thumbnail folder — and a
//! generation written onto the database file. A path is only a locator; at
//! start-up the objects there must be these, or nothing is opened
//! ([`crate::binding`]). Bound choices are written as format 2, so a build
//! that does not know bindings refuses the file ("newer format") instead of
//! silently using the path alone. Choices without a binding — written by
//! older builds, the first launch, or an explicit "use this existing
//! folder" — are read and used as before; they were never a proven copy and
//! are not declared one.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::StartupError;

/// The newest format this build reads and writes: 2 when a choice carries a
/// [`Binding`], 1 otherwise. A newer one was written by a newer build; it is
/// shown as an error and left alone rather than rewritten.
pub const BOOTSTRAP_VERSION: u32 = 2;
/// Choices without bindings only.
const UNBOUND_VERSION: u32 = 1;

/// What a moved data folder was when it was proven and chosen. Compared at
/// every start before the database is opened ([`crate::binding`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// The volume's own identity (UUID on macOS, file system ID on Linux) —
    /// not a device number, which changes when a disk is attached again.
    pub volume: String,
    /// Inode numbers on that volume.
    pub dir: u64,
    pub db: u64,
    pub thumbs: u64,
    /// Written onto the database file (an extended attribute) when it was
    /// copied. With the inode it tells a recycled inode number from the
    /// copy; alone it would prove nothing, since it can be copied too.
    pub generation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoredMode {
    /// `<app_local_data>/data`.
    System,
    /// An absolute directory the user picked.
    Custom,
}

/// One remembered choice of data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub mode: StoredMode,
    /// Absolute for `custom`, `null` for `system`.
    pub data_dir: Option<PathBuf>,
    /// Set for a folder this app moved the data into and proved.
    /// Boxed: it is rarely there, and errors carry choices around.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<Box<Binding>>,
}

impl Choice {
    pub fn system() -> Self {
        Self {
            mode: StoredMode::System,
            data_dir: None,
            binding: None,
        }
    }

    pub fn custom(dir: PathBuf) -> Self {
        Self {
            mode: StoredMode::Custom,
            data_dir: Some(dir),
            binding: None,
        }
    }

    /// The same choice, bound to the objects it must lead to.
    pub fn bound(self, binding: Binding) -> Self {
        Self {
            binding: Some(Box::new(binding)),
            ..self
        }
    }

    /// Why this choice cannot be used as written, if it cannot.
    fn defect(&self) -> Option<String> {
        match (self.mode, &self.data_dir) {
            (StoredMode::System, None) => None,
            (StoredMode::System, Some(_)) => Some("mode \"system\" with a data_dir".into()),
            (StoredMode::Custom, None) => Some("mode \"custom\" without a data_dir".into()),
            // A relative path would be resolved against whatever the current
            // directory happens to be, which for an app started from Finder
            // or Explorer is anything at all.
            (StoredMode::Custom, Some(d)) if !d.is_absolute() => {
                Some(format!("data_dir is not absolute: {}", d.display()))
            }
            (StoredMode::Custom, Some(_)) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub version: u32,
    #[serde(flatten)]
    pub current: Choice,
    #[serde(default)]
    pub previous: Option<Choice>,
}

impl Bootstrap {
    pub fn new(current: Choice, previous: Option<Choice>) -> Self {
        let mut b = Self {
            version: UNBOUND_VERSION,
            current,
            previous,
        };
        b.version = b.format();
        b
    }

    /// The oldest format that can hold these choices.
    fn format(&self) -> u32 {
        let bound = std::iter::once(&self.current)
            .chain(self.previous.as_ref())
            .any(|c| c.binding.is_some());
        if bound {
            BOOTSTRAP_VERSION
        } else {
            UNBOUND_VERSION
        }
    }
}

/// Read the bootstrap. `Ok(None)` means there is none — a first launch.
///
/// Anything else that goes wrong is an error, never "no bootstrap": treating
/// an unreadable file as absent would start a first run, create an empty
/// database in the system directory and then overwrite the very file that
/// said the data was on another disk.
pub fn read_bootstrap(path: &Path) -> Result<Option<Bootstrap>, StartupError> {
    let unreadable = |reason: String| StartupError::BootstrapUnreadable {
        path: path.to_path_buf(),
        reason,
    };
    let text = match fs::read(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    // Look at the version first, so a newer format with fields this build
    // does not know is reported as newer rather than as corrupt.
    let raw: serde_json::Value =
        serde_json::from_slice(&text).map_err(|e| unreadable(e.to_string()))?;
    match raw.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(BOOTSTRAP_VERSION) || v == u64::from(UNBOUND_VERSION) => {}
        Some(v) => {
            return Err(StartupError::BootstrapUnsupported {
                path: path.to_path_buf(),
                version: v,
            })
        }
        None => return Err(unreadable("no version".into())),
    }
    let b: Bootstrap = serde_json::from_value(raw).map_err(|e| unreadable(e.to_string()))?;
    if b.version < b.format() {
        return Err(unreadable("a binding in a format 1 file".into()));
    }
    for choice in std::iter::once(&b.current).chain(b.previous.as_ref()) {
        if let Some(defect) = choice.defect() {
            return Err(unreadable(defect));
        }
    }
    Ok(Some(b))
}

/// Replace the bootstrap atomically: a temporary file beside it, flushed,
/// then renamed over. A crash leaves either the old file or the new one,
/// never half of either.
pub fn write_bootstrap(path: &Path, b: &Bootstrap) -> Result<(), StartupError> {
    write_bootstrap_checked(path, b, || Ok::<(), StartupError>(()))
}

/// [`write_bootstrap`] with `last_check` run after the new file is written
/// and flushed, right before it replaces the old one: nothing that can
/// wait on something outside (reading, writing, flushing) comes between the
/// check and the replacement. If the check fails the new file is removed
/// and the bootstrap stays exactly as it was.
pub(crate) fn write_bootstrap_checked<E: From<StartupError>>(
    path: &Path,
    b: &Bootstrap,
    last_check: impl FnOnce() -> Result<(), E>,
) -> Result<(), E> {
    let mut b = b.clone();
    b.version = b.format();
    let b = &b;
    let failed = |reason: String| StartupError::BootstrapWrite {
        path: path.to_path_buf(),
        reason,
    };
    for choice in std::iter::once(&b.current).chain(b.previous.as_ref()) {
        if let Some(defect) = choice.defect() {
            return Err(failed(defect).into());
        }
    }
    // Non-UTF-8 paths cannot be written as JSON strings; that surfaces here
    // as an error instead of as a file that reads back differently.
    let text = serde_json::to_vec_pretty(b).map_err(|e| failed(e.to_string()))?;
    let dir = path
        .parent()
        .ok_or_else(|| failed("no parent directory".into()))?;
    fs::create_dir_all(dir).map_err(|e| failed(e.to_string()))?;
    let (tmp, mut file) = create_temporary(path).map_err(|e| failed(e.to_string()))?;
    let written = (|| {
        file.write_all(&text)?;
        file.write_all(b"\n")?;
        file.sync_all()
    })();
    if let Err(e) = written {
        remove_own_temporary(&tmp, &file);
        return Err(failed(e.to_string()).into());
    }
    if let Err(e) = last_check() {
        remove_own_temporary(&tmp, &file);
        return Err(e);
    }
    // On Windows too `fs::rename` replaces an existing target
    // (MoveFileExW with MOVEFILE_REPLACE_EXISTING). The rename replaces the
    // bootstrap's *name*; a link standing there is replaced, not written
    // through.
    if let Err(e) = fs::rename(&tmp, path) {
        remove_own_temporary(&tmp, &file);
        return Err(failed(e.to_string()).into());
    }
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// How many names [`create_temporary`] tries before giving up.
const TEMPORARY_ATTEMPTS: u32 = 16;

/// A new temporary file beside the bootstrap, created by this call.
///
/// It used to be one fixed name, `desktop.json.tmp`, opened with
/// `File::create`: whatever already stood there was truncated and
/// written — through a symbolic link into the file it pointed to, through a
/// hard link into another name's file. Now every attempt takes a fresh
/// name and creates it exclusively (`create_new`: it fails on any existing
/// entry, a link included, and never follows one). An existing entry is
/// never opened, truncated or removed; the next name is tried.
fn create_temporary(path: &Path) -> std::io::Result<(std::path::PathBuf, fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = path.file_name().unwrap_or_default().to_os_string();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for _ in 0..TEMPORARY_ATTEMPTS {
        let mut name = base.clone();
        name.push(format!(
            ".{}-{nanos:08x}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let tmp = path.with_file_name(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free name for the bootstrap's temporary file",
    ))
}

/// Remove the temporary file this call created — only while its name still
/// holds that very file. Anything else found there is left alone (and the
/// file, if it went elsewhere, is left as a harmless stray).
fn remove_own_temporary(tmp: &Path, file: &fs::File) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (Ok(now), Ok(ours)) = (fs::symlink_metadata(tmp), file.metadata()) else {
            return;
        };
        if (now.dev(), now.ino()) != (ours.dev(), ours.ino()) {
            return;
        }
    }
    // Elsewhere the file identity is not compared: the name is this call's
    // own, unique and just created, and nothing but this user can replace
    // it in the settings folder.
    #[cfg(not(unix))]
    let _ = file;
    let _ = fs::remove_file(tmp);
}
