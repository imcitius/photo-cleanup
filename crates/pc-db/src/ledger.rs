//! Where the thumbnail cache records its active generation (el-5x1uh, review
//! el-19kbm): a row of `thumb_generations` in the application's database,
//! never a file inside the cache that another program could replace.

use crate::Db;
use anyhow::Result;
use rusqlite::OptionalExtension;
use std::path::{Path, PathBuf};

/// The key a cache folder is recorded under: its real path (links and
/// `..` resolved), so the server and the command line, started from
/// different folders or given the path through a link, find the same row.
/// A folder not there yet is keyed by its parent's real path and its name —
/// what it will resolve to once made.
fn key(root: &Path) -> String {
    let absolute = std::path::absolute(root).unwrap_or_else(|_| root.to_path_buf());
    let real = std::fs::canonicalize(&absolute)
        .ok()
        .or_else(|| {
            let parent = std::fs::canonicalize(absolute.parent()?).ok()?;
            Some(parent.join(absolute.file_name()?))
        })
        .unwrap_or(absolute);
    real.to_string_lossy().into_owned()
}

impl Db {
    /// The generation recorded for the thumbnail cache at `root`.
    pub fn thumb_generation(&self, root: &Path) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM thumb_generations WHERE root = ?1",
                [key(root)],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Record `value` for the cache at `root` if what is recorded is still
    /// `previous` (`None`: nothing). `false`: another value was recorded
    /// meanwhile, and nothing was changed.
    pub fn record_thumb_generation(
        &self,
        root: &Path,
        previous: Option<&str>,
        value: &str,
    ) -> Result<bool> {
        let root = key(root);
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let now: Option<String> = tx
            .query_row(
                "SELECT value FROM thumb_generations WHERE root = ?1",
                [&root],
                |r| r.get(0),
            )
            .optional()?;
        if now.as_deref() != previous {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO thumb_generations(root, value) VALUES (?1, ?2)
             ON CONFLICT(root) DO UPDATE SET value = excluded.value",
            [&root, value],
        )?;
        tx.commit()?;
        Ok(true)
    }
}

/// [`pc_core::thumbstore::GenerationLedger`] over the application's
/// database. Each question opens its own connection with `open` (the same
/// way the application opens the database, so a bound data folder is
/// confirmed first): the store may ask while a request holds the shared
/// connection, and asks rarely — at its first use and at a reset.
pub struct ThumbLedger {
    open: Box<dyn Fn() -> Result<Db> + Send + Sync>,
    /// For `Debug`.
    path: PathBuf,
}

impl ThumbLedger {
    /// A ledger in the database at `path`, opened with [`Db::open`].
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let p = path.clone();
        Self::with(path, move || Db::open(&p))
    }

    /// A ledger in the database `open` returns (for a bound data folder,
    /// [`Db::open_bound`]).
    pub fn with(
        path: impl Into<PathBuf>,
        open: impl Fn() -> Result<Db> + Send + Sync + 'static,
    ) -> Self {
        Self {
            open: Box::new(open),
            path: path.into(),
        }
    }
}

impl std::fmt::Debug for ThumbLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbLedger")
            .field("path", &self.path)
            .finish()
    }
}

impl pc_core::thumbstore::GenerationLedger for ThumbLedger {
    fn recorded(&self, root: &Path) -> Result<Option<String>> {
        (self.open)()?.thumb_generation(root)
    }

    fn record(&self, root: &Path, previous: Option<&str>, value: &str) -> Result<bool> {
        (self.open)()?.record_thumb_generation(root, previous, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generation_is_recorded_only_over_the_value_the_caller_saw() {
        let db = Db::open_in_memory().unwrap();
        let root = Path::new("/cache/thumbs");
        assert_eq!(db.thumb_generation(root).unwrap(), None);
        assert!(db.record_thumb_generation(root, None, "a").unwrap());
        // Somebody who still believes nothing is recorded changes nothing.
        assert!(!db.record_thumb_generation(root, None, "b").unwrap());
        assert!(!db.record_thumb_generation(root, Some("x"), "b").unwrap());
        assert_eq!(db.thumb_generation(root).unwrap().as_deref(), Some("a"));
        assert!(db.record_thumb_generation(root, Some("a"), "c").unwrap());
        assert_eq!(db.thumb_generation(root).unwrap().as_deref(), Some("c"));
        // The same folder named another way is the same row.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("thumbs");
        std::fs::create_dir(&dir).unwrap();
        assert!(db.record_thumb_generation(&dir, None, "d").unwrap());
        let other_name = tmp.path().join("thumbs/../thumbs");
        assert_eq!(
            db.thumb_generation(&other_name).unwrap().as_deref(),
            Some("d")
        );
        // Another cache folder has its own row.
        assert_eq!(db.thumb_generation(Path::new("/other")).unwrap(), None);
    }

    #[test]
    fn a_reset_of_the_index_keeps_the_recorded_generation() {
        let db = Db::open_in_memory().unwrap();
        let root = Path::new("/cache/thumbs");
        assert!(db.record_thumb_generation(root, None, "a").unwrap());
        db.reset_index().unwrap();
        assert_eq!(db.thumb_generation(root).unwrap().as_deref(), Some("a"));
    }
}
