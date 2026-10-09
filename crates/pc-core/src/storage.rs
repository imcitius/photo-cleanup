//! A data folder bound to proven objects, for everything that writes there.
//!
//! The server, SQLite and the thumbnail cache all work with paths. For most
//! data folders that is all there is. A folder the desktop app moved the
//! data into is *bound*: the database file and the thumbnail folder in it
//! were proven to be this program's copy, and the path is only a locator
//! for those objects. Somebody may replace one of them afterwards — restore
//! an old folder from a backup, drop another database in its place — and a
//! write by path would then land in the replacement: SQLite would migrate a
//! foreign file, a reset would empty a foreign folder.
//!
//! So every writer asks the binding right before it writes, and a refusal
//! stops the write with nothing done. The binding is owned by the program
//! that proved the folder (`pc-desktop`); this crate only carries the
//! question to where the writes happen.
//!
//! What the check can promise depends on who can rename entries in the
//! folder. The desktop admits a bound folder only in a namespace nobody but
//! this user and the administrator can change. A replacement made by
//! ordinary means — the user's own programs, a sync tool, a restored backup
//! — at any moment the user can act in is refused before it is written to.
//! What is left is the few system calls between a check and the write it
//! guards (check → open, check → write, check → unlink): only a process
//! running as this user (or root/admin) could swap an object there, and
//! doing so on purpose is what DESKTOP.md excludes from the threat model —
//! a *deliberately malicious* process with this user's UID, root or admin,
//! which could change these files directly anyway. Ordinary programs of
//! this user are inside the model.

use std::fmt;
use std::sync::Arc;

/// Asked right before a write, never after.
pub trait StorageBinding: Send + Sync + fmt::Debug {
    /// The database file at its path is still the proven one, protected,
    /// and nothing that SQLite or the writer lock would open beside it
    /// (`-wal`, `-shm`, `-journal`, `.writer-lock`) is a link or somebody
    /// else's file. Called before the database is opened and before the
    /// writer lock is taken.
    fn check_database(&self) -> Result<(), String>;

    /// The thumbnail folder at its path is still the proven one and nobody
    /// else can write to it. Called before anything in the cache is
    /// written or removed.
    fn check_thumbnails(&self) -> Result<(), String>;

    /// [`StorageBinding::check_thumbnails`], and `folder` — the cache as
    /// the caller holds it open — is the proven folder itself. What the
    /// caller then does through that descriptor happens in the proven
    /// folder, whatever bears its name afterwards. Called before anything
    /// is removed from the cache (`ThumbStore::clear`) and before a
    /// thumbnail is created in it.
    fn check_thumbnail_folder(&self, folder: &std::fs::File) -> Result<(), String>;
}

/// Shared by the server state, its jobs and the thumbnail cache.
pub type Binding = Arc<dyn StorageBinding>;

/// The error a refused write surfaces as.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NotBound(pub String);
