use anyhow::Result;
use pc_core::ThumbStore;
use pc_db::Db;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Requests share a read connection; background work opens its own connection.
/// The mutation gate serializes jobs and manual changes across both connections.
pub struct AppState {
    pub db: Mutex<Db>,
    pub jobs: crate::jobs::Jobs,
    pub mutation: Mutex<()>,
    pub network: bool,
    pub thumbs: ThumbStore,
    pub db_path: PathBuf,
    /// Where moved files are parked. `None` means the default: the root of
    /// each file's own filesystem, which keeps the move a rename.
    pub quarantine: Option<PathBuf>,
}

impl AppState {
    pub fn new(db_path: &Path, thumbs: &Path, quarantine: Option<PathBuf>) -> Result<Self> {
        let db_path = std::path::absolute(db_path)?;
        let thumbs = std::path::absolute(thumbs)?;
        let quarantine = quarantine.map(std::path::absolute).transpose()?;
        let db = Db::open(&db_path)?;
        db.conn.execute("UPDATE jobs SET state='interrupted', finished_at=?1, error='Сервер перезапущен. Проверьте журнал перед новым запуском.' WHERE state IN ('queued','running')",[pc_core::time::now_unix()])?;
        Ok(Self {
            jobs: Default::default(),
            mutation: Mutex::new(()),
            network: false,
            db: Mutex::new(db),
            thumbs: ThumbStore::new(thumbs),
            db_path,
            quarantine,
        })
    }
}
