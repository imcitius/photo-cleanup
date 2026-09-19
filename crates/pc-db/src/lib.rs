//! SQLite storage: schema, migrations and the queries phase 0 needs.

pub mod files;
pub mod model;
pub mod schema;

pub use files::{FileRow, IndexStats, NewFile, NewMeta};
pub use model::{
    Bundle, BundleState, Catalog, JournalEntry, JournalStatus, NewBundle, NewCatalog,
    NewJournalEntry,
};

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub struct Db {
    pub conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        let conn = Connection::open(path)
            .with_context(|| format!("не удалось открыть базу {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // 16 MiB page cache: the archive is small enough that this is plenty
        // and it keeps us well inside the 4-6 GiB ceiling the design sets.
        conn.pragma_update(None, "cache_size", -16_384)?;
        let db = Self { conn };
        schema::migrate(&db.conn)?;
        Ok(db)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn start_run(&self, roots: &[String], tool_version: &str) -> Result<i64> {
        let roots_json = serde_json::to_string(roots)?;
        self.conn.execute(
            "INSERT INTO runs(started_at, roots, tool_version) VALUES (?1, ?2, ?3)",
            rusqlite::params![pc_core::time::now_unix(), roots_json, tool_version],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(&self, run_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET finished_at = ?1 WHERE id = ?2",
            rusqlite::params![pc_core::time::now_unix(), run_id],
        )?;
        Ok(())
    }

    pub fn latest_run(&self) -> Result<Option<i64>> {
        let id = self
            .conn
            .query_row("SELECT id FROM runs ORDER BY id DESC LIMIT 1", [], |r| {
                r.get::<_, i64>(0)
            })
            .ok();
        Ok(id)
    }
}
