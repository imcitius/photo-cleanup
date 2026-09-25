//! Why a launch cannot go ahead.
//!
//! Every variant is a screen in the shell, not a log line: the app has no
//! console. The type serialises with a `kind` tag so the shell (and the web
//! UI behind it) can offer the right actions — retry, pick another folder,
//! go back to the previous one — without parsing prose.

use serde::Serialize;
use std::fmt;
use std::path::PathBuf;

use crate::bootstrap::Choice;
use crate::resolve::Source;

/// What is wrong with a data directory that should hold an archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "what", content = "reason")]
pub enum Unavailable {
    /// The directory is not there — typically an unplugged drive.
    Missing,
    NotADirectory,
    /// The directory is there, the database is not.
    NoDatabase,
    /// Something is at the database path but SQLite cannot use it.
    NotADatabase(String),
    /// The directory or database cannot be written. SQLite in WAL mode
    /// needs the directory itself writable, not just the file.
    NotWritable(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StartupError {
    /// The bootstrap exists but cannot be read or understood. It is left
    /// exactly as it is.
    BootstrapUnreadable {
        path: PathBuf,
        reason: String,
    },
    /// Written by a newer build. Left alone.
    BootstrapUnsupported {
        path: PathBuf,
        version: u64,
    },
    BootstrapWrite {
        path: PathBuf,
        reason: String,
    },
    /// The data directory this launch must use cannot be used. `previous`
    /// is offered as "go back" when the bootstrap remembers one.
    DataUnavailable {
        source: Source,
        dir: PathBuf,
        why: Unavailable,
        previous: Option<Choice>,
    },
    /// A folder picked for a new archive already holds one. It is never
    /// overwritten; switching to it is a separate, explicit action.
    DatabaseExists {
        dir: PathBuf,
    },
    /// A folder picked for the data must be given as an absolute path.
    RelativePath {
        path: PathBuf,
    },
    /// Portable mode decides the data directory; a choice in the user
    /// profile would be silently ignored, so it is refused instead.
    PortableActive {
        marker: PathBuf,
    },
    /// "Go back" was asked for with nothing to go back to.
    NoPrevious,
    /// Creating a new database failed.
    CreateFailed {
        dir: PathBuf,
        reason: String,
    },
}

impl std::error::Error for StartupError {}

impl fmt::Display for Unavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Missing => pc_core::tr!("папки нет", "the folder does not exist").to_string(),
            Self::NotADirectory => pc_core::tr!("это не папка", "it is not a folder").to_string(),
            Self::NoDatabase => {
                pc_core::tf!("в папке нет {0}", "there is no {0} in it", crate::DB_FILE)
            }
            Self::NotADatabase(r) => pc_core::tf!(
                "база не открывается: {0}",
                "the database cannot be opened: {0}",
                r
            ),
            Self::NotWritable(r) => {
                pc_core::tf!("нет доступа на запись: {0}", "cannot write there: {0}", r)
            }
        };
        f.write_str(&s)
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::BootstrapUnreadable { path, reason } => pc_core::tf!(
                "не читается файл настроек {0}: {1}",
                "cannot read the settings file {0}: {1}",
                path.display(),
                reason
            ),
            Self::BootstrapUnsupported { path, version } => pc_core::tf!(
                "файл настроек {0} записан более новой версией (формат {1})",
                "the settings file {0} was written by a newer version (format {1})",
                path.display(),
                version
            ),
            Self::BootstrapWrite { path, reason } => pc_core::tf!(
                "не записать файл настроек {0}: {1}",
                "cannot write the settings file {0}: {1}",
                path.display(),
                reason
            ),
            Self::DataUnavailable { dir, why, .. } => pc_core::tf!(
                "данные недоступны: {0} — {1}",
                "data unavailable: {0} — {1}",
                dir.display(),
                why
            ),
            Self::DatabaseExists { dir } => pc_core::tf!(
                "в {0} уже есть база; она не перезаписывается",
                "{0} already holds a database; it is not overwritten",
                dir.display()
            ),
            Self::RelativePath { path } => pc_core::tf!(
                "нужен полный путь, а не {0}",
                "a full path is needed, not {0}",
                path.display()
            ),
            Self::PortableActive { marker } => pc_core::tf!(
                "включён переносной режим ({0}); папка данных — рядом с программой",
                "portable mode is on ({0}); the data folder is next to the program",
                marker.display()
            ),
            Self::NoPrevious => pc_core::tr!(
                "прежней папки данных нет",
                "there is no previous data folder"
            )
            .to_string(),
            Self::CreateFailed { dir, reason } => pc_core::tf!(
                "не создать базу в {0}: {1}",
                "cannot create a database in {0}: {1}",
                dir.display(),
                reason
            ),
        };
        f.write_str(&s)
    }
}
