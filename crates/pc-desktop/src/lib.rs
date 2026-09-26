//! Where the desktop app keeps its data, and how it finds it again.
//!
//! The database cannot say where the database is, so the choice lives in a
//! small bootstrap file outside both the data directory and the program
//! directory. Everything here is platform-neutral: the system directories are
//! handed in as [`SystemDirs`] rather than asked of Tauri, so the rules are
//! tested on every OS without a window.
//!
//! The contract with the shell is three calls:
//!
//! 1. [`resolve`] reads the bootstrap and the disk and says which data
//!    directory this launch uses. It writes nothing.
//! 2. [`prepare`] opens (or, where that is allowed, creates) the database and
//!    only then records the choice. The shell starts `pc-api` on
//!    [`Prepared::layout`].
//! 3. [`confirm_started`] after the server is up: a choice waiting for its
//!    first successful launch stops being provisional.
//!
//! The window's own rules — which pages it may show, the address it opens,
//! the report it shows when start-up fails — are in the same neutral half
//! ([`is_server_page`], [`StartupReport`]); the Tauri shell in `main.rs` only
//! applies them, on Windows and macOS.
//!
//! A failure at any step is a [`StartupError`] the shell shows as a screen.
//! None of them falls back to an empty database somewhere else: a user whose
//! external drive is unplugged must see "your data is not there", not a
//! fresh, empty archive that looks as if everything was lost.

mod bootstrap;
mod error;
mod native;
mod relocate;
mod resolve;
mod window;

pub use bootstrap::{read_bootstrap, write_bootstrap, Bootstrap, Choice, StoredMode};
pub use error::{StartupError, Unavailable};
pub use native::{
    desktop_info, interface_origin, permission, ChangeAction, DesktopInfo, ERROR_PAGE_COMMANDS,
    INTERFACE_COMMANDS,
};
pub use relocate::{
    copy_data, measure, preview_move, preview_move_with, restart_or_restore, switch_to_existing,
    verify, Blocker, Copied, DataSize, MovePreview, RelocateError, PARTIAL_DB, PARTIAL_THUMBS,
    SPACE_MARGIN,
};
pub use resolve::{
    choose_data_dir, confirm_started, prepare, resolve, revert_to_previous, Creation, NewDir,
    Prepared, Resolved, Source,
};
pub use window::{
    is_bundled_page, is_server_page, parse_args, server_url, StartupReport, ERROR_PAGE,
    SERVER_BIND, WINDOW_LABEL, WINDOW_MIN_SIZE, WINDOW_SIZE, WINDOW_TITLE,
};

use serde::Serialize;
use std::path::{Path, PathBuf};

/// Tauri's identifier, and therefore the name of the per-user directory.
pub const APP_ID: &str = "io.github.imcitius.photo-cleanup";
/// The bootstrap file, inside the per-user app directory.
pub const BOOTSTRAP_FILE: &str = "desktop.json";
/// Present next to the executable, it asks for portable mode (Windows only).
pub const PORTABLE_MARKER: &str = "photo-cleanup.portable";
/// The database file inside a data directory. The same name the Windows
/// batch file and the CLI default use, so a folder of either is recognised.
pub const DB_FILE: &str = "photo-cleanup.db";
/// The thumbnail cache inside a data directory — where the CLI puts it too.
pub const THUMBS_DIR: &str = "thumbs";
/// The data directory under the per-user app directory, and under the
/// executable's directory in portable mode.
pub const DATA_SUBDIR: &str = "data";

/// What the operating system says about where things are.
///
/// The shell fills this from Tauri (`app_local_data_dir()`,
/// `current_exe()`); tests fill it with temporary directories.
#[derive(Debug, Clone)]
pub struct SystemDirs {
    /// `%LOCALAPPDATA%\<id>` on Windows, `~/Library/Application
    /// Support/<id>` on macOS. The roaming profile is not used: a path to a
    /// local disk carried to another machine would be a lie.
    pub app_local_data: PathBuf,
    /// The directory holding the executable, if known. Only ever read,
    /// except in explicit portable mode.
    pub exe_dir: Option<PathBuf>,
    /// Whether the portable marker is honoured. True on Windows only: a
    /// downloaded macOS `.app` runs from a randomised read-only location
    /// (App Translocation), so "next to the program" does not exist there.
    pub portable_supported: bool,
}

impl SystemDirs {
    /// The directories with this platform's portable rule.
    pub fn new(app_local_data: PathBuf, exe_dir: Option<PathBuf>) -> Self {
        Self {
            app_local_data,
            exe_dir,
            portable_supported: cfg!(windows),
        }
    }

    pub fn bootstrap_path(&self) -> PathBuf {
        self.app_local_data.join(BOOTSTRAP_FILE)
    }

    pub fn system_data_dir(&self) -> PathBuf {
        self.app_local_data.join(DATA_SUBDIR)
    }

    /// The marker file, when portable mode is supported here and asked for.
    pub fn portable_marker(&self) -> Option<PathBuf> {
        if !self.portable_supported {
            return None;
        }
        let marker = self.exe_dir.as_ref()?.join(PORTABLE_MARKER);
        marker.is_file().then_some(marker)
    }
}

/// The files of one data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataLayout {
    pub dir: PathBuf,
    /// Plus `-wal`, `-shm` and `.writer-lock` beside it while in use.
    pub db: PathBuf,
    pub thumbs: PathBuf,
}

impl DataLayout {
    pub fn in_dir(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            db: dir.join(DB_FILE),
            thumbs: dir.join(THUMBS_DIR),
        }
    }
}
