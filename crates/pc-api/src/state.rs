use anyhow::Result;
use pc_core::ThumbStore;
use pc_db::Db;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// SQLite takes one writer and this tool has one user, so a single
/// connection behind a mutex is both sufficient and easier to reason about
/// than a pool.
pub struct AppState {
    pub db: Mutex<Db>,
    pub thumbs: ThumbStore,
    pub db_path: PathBuf,
}

impl AppState {
    pub fn new(db_path: &Path, thumbs: &Path) -> Result<Self> {
        Ok(Self {
            db: Mutex::new(Db::open(db_path)?),
            thumbs: ThumbStore::new(thumbs),
            db_path: db_path.to_path_buf(),
        })
    }
}
