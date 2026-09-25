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

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::StartupError;

/// The only format this build reads. A newer one was written by a newer
/// build; it is shown as an error and left alone rather than rewritten.
pub const BOOTSTRAP_VERSION: u32 = 1;

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
}

impl Choice {
    pub fn system() -> Self {
        Self {
            mode: StoredMode::System,
            data_dir: None,
        }
    }

    pub fn custom(dir: PathBuf) -> Self {
        Self {
            mode: StoredMode::Custom,
            data_dir: Some(dir),
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
        Self {
            version: BOOTSTRAP_VERSION,
            current,
            previous,
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
        Some(v) if v == u64::from(BOOTSTRAP_VERSION) => {}
        Some(v) => {
            return Err(StartupError::BootstrapUnsupported {
                path: path.to_path_buf(),
                version: v,
            })
        }
        None => return Err(unreadable("no version".into())),
    }
    let b: Bootstrap = serde_json::from_value(raw).map_err(|e| unreadable(e.to_string()))?;
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
    let failed = |reason: String| StartupError::BootstrapWrite {
        path: path.to_path_buf(),
        reason,
    };
    for choice in std::iter::once(&b.current).chain(b.previous.as_ref()) {
        if let Some(defect) = choice.defect() {
            return Err(failed(defect));
        }
    }
    // Non-UTF-8 paths cannot be written as JSON strings; that surfaces here
    // as an error instead of as a file that reads back differently.
    let text = serde_json::to_vec_pretty(b).map_err(|e| failed(e.to_string()))?;
    let dir = path
        .parent()
        .ok_or_else(|| failed("no parent directory".into()))?;
    fs::create_dir_all(dir).map_err(|e| failed(e.to_string()))?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&text)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        // On Windows too `fs::rename` replaces an existing target
        // (MoveFileExW with MOVEFILE_REPLACE_EXISTING).
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(failed(e.to_string()));
    }
    Ok(())
}
