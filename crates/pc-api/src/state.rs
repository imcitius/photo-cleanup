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
        // A job still marked as running belongs to a process that is gone —
        // unless it does not. A second server on the same database sees the
        // first one's live work here, and calling it interrupted is a lie
        // told about a job that is at that moment moving files.
        //
        // The writer's lock answers this: if it can be taken, nobody is
        // writing, so whatever is still marked running stopped without
        // saying so. It is let go again at once; this is a question, not a
        // claim on the archive.
        match pc_core::lock::take_writer(&db_path, "") {
            Ok(writer) => {
                db.conn.execute(
                    "UPDATE jobs SET state='interrupted', finished_at=?1, error=?2
                      WHERE state IN ('queued','running')",
                    rusqlite::params![
                        pc_core::time::now_unix(),
                        pc_core::tr!(
                            "Сервер перезапущен. Проверьте журнал перед новым запуском.",
                            "The server restarted. Check the journal before starting again."
                        )
                    ],
                )?;
                drop(writer);
            }
            Err(e) if e.is::<pc_core::lock::Busy>() => {
                tracing::info!("{e:#}");
            }
            Err(e) => return Err(e),
        }
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
