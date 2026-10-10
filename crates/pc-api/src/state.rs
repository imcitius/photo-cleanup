use anyhow::Result;
use pc_core::storage::Binding;
use pc_core::ThumbStore;
use pc_db::Db;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

/// Requests share a read connection; background work opens its own connection.
/// The mutation gate serializes jobs and manual changes across both connections.
pub struct AppState {
    pub db: Mutex<Db>,
    pub jobs: crate::jobs::Jobs,
    pub mutation: Mutex<()>,
    pub network: bool,
    /// Set once a controlled shutdown has begun. Every writer checks it under
    /// the mutation gate, so after the shutdown has passed that gate once, no
    /// new job and no hand-made change can start: it gets a 503 instead.
    pub closing: AtomicBool,
    pub thumbs: ThumbStore,
    pub db_path: PathBuf,
    /// Where moved files are parked. `None` means the default: the root of
    /// each file's own filesystem, which keeps the move a rename.
    pub quarantine: Option<PathBuf>,
    /// A bound data folder's proof ([`pc_core::storage`]): asked before
    /// every database open, writer lock and thumbnail write or removal.
    pub binding: Option<Binding>,
}

/// Open the database the way this server does: through the binding when
/// there is one, so a replaced file is refused before SQLite writes to it.
pub fn open_db(path: &Path, binding: Option<&Binding>) -> Result<Db> {
    match binding {
        None => Db::open(path),
        Some(b) => Db::open_bound(path, b.as_ref()),
    }
}

impl AppState {
    pub fn new(db_path: &Path, thumbs: &Path, quarantine: Option<PathBuf>) -> Result<Self> {
        Self::open(db_path, thumbs, quarantine, None)
    }

    /// [`AppState::new`], with a bound data folder's proof when there is one.
    pub fn open(
        db_path: &Path,
        thumbs: &Path,
        quarantine: Option<PathBuf>,
        binding: Option<Binding>,
    ) -> Result<Self> {
        let db_path = std::path::absolute(db_path)?;
        let thumbs = std::path::absolute(thumbs)?;
        let quarantine = quarantine.map(std::path::absolute).transpose()?;
        let db = open_db(&db_path, binding.as_ref())?;
        // A job still marked as running belongs to a process that is gone —
        // unless it does not. A second server on the same database sees the
        // first one's live work here, and calling it interrupted is a lie
        // told about a job that is at that moment moving files.
        //
        // The writer's lock answers this: if it can be taken, nobody is
        // writing, so whatever is still marked running stopped without
        // saying so. It is let go again at once; this is a question, not a
        // claim on the archive.
        match take_writer(&db_path, binding.as_ref(), "") {
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
            closing: AtomicBool::new(false),
            db: Mutex::new(db),
            thumbs: thumb_store(&db_path, thumbs, binding.as_ref()),
            db_path,
            quarantine,
            binding,
        })
    }

    pub fn open_db(&self) -> Result<Db> {
        open_db(&self.db_path, self.binding.as_ref())
    }

    pub fn take_writer(&self, what: &str) -> Result<pc_core::lock::WriterLock> {
        take_writer(&self.db_path, self.binding.as_ref(), what)
    }
}

/// The thumbnail cache at `thumbs`, its active generation recorded in the
/// database at `db_path` (opened the way this server opens it), confirmed
/// through `binding` when there is one.
pub fn thumb_store(db_path: &Path, thumbs: PathBuf, binding: Option<&Binding>) -> ThumbStore {
    let store = match binding {
        None => ThumbStore::new(thumbs),
        Some(b) => ThumbStore::bound(thumbs, b.clone()),
    };
    let (path, binding) = (db_path.to_path_buf(), binding.cloned());
    let ledger = pc_db::ThumbLedger::with(db_path, move || open_db(&path, binding.as_ref()));
    store.with_ledger(std::sync::Arc::new(ledger))
}

/// The writer lock lives beside the database, so a bound folder is
/// confirmed before its file is opened and written.
fn take_writer(
    db_path: &Path,
    binding: Option<&Binding>,
    what: &str,
) -> Result<pc_core::lock::WriterLock> {
    if let Some(b) = binding {
        b.check_database().map_err(pc_core::storage::NotBound)?;
    }
    pc_core::lock::take_writer(db_path, what)
}
