//! SQLite storage: schema, migrations and the queries phase 0 needs.

pub mod files;
pub mod model;
pub mod organize;
pub mod schema;

pub use files::{
    FamilyRow, FileInfo, FileRow, IndexStats, MemberRow, NewFile, NewMeta, PlanRow,
    SeriesMemberRow, SeriesRow,
};
pub use model::{
    Bundle, BundleState, Catalog, JournalEntry, JournalStatus, NewBundle, NewCatalog,
    NewJournalEntry,
};
pub use organize::OrganizeRow;

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
        let conn = Connection::open(path).with_context(|| {
            pc_core::tf!(
                "не удалось открыть базу {0}",
                "could not open the database {0}",
                path.display()
            )
        })?;
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

    /// Throw the index away and start over.
    ///
    /// Everything that was *derived* from the archive goes: the file rows,
    /// their metadata, families, series, categories and the hand corrections
    /// that hang off them. Two things deliberately stay. Settings, because
    /// nobody means "forget which folders I chose" when they say "rescan".
    /// And the journal with its runs, because those rows are the only record
    /// of files this tool actually moved — wiping them would strand whatever
    /// sits in quarantine with no way back.
    ///
    /// The thumbnail cache is separate from the database and is cleared by
    /// its owner; see `ThumbStore::clear`.
    pub fn reset_index(&self) -> Result<()> {
        // Children before parents: the manual tables reference files(id)
        // without a cascade, on purpose, so a stray delete cannot quietly
        // take a curator's decisions with it.
        self.conn.execute_batch(
            "BEGIN;
             DELETE FROM manual_keepers;
             DELETE FROM manual_splits;
             DELETE FROM manual_best;
             DELETE FROM manual_rejects;
             DELETE FROM file_categories;
             DELETE FROM series_members;
             DELETE FROM series;
             DELETE FROM family_members;
             DELETE FROM families;
             DELETE FROM meta;
             DELETE FROM files;
             DELETE FROM lr_files;
             DELETE FROM lr_catalogs;
             DELETE FROM derived_bundles;
             DELETE FROM jobs;
             COMMIT;",
        )?;
        // Outside the transaction: SQLite will not vacuum inside one.
        self.conn.execute_batch("VACUUM")?;
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

/// The clock, in one place, so the database layer does not reach for it in
/// five.
pub(crate) fn pc_core_now() -> i64 {
    pc_core::time::now_unix()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_file(db: &Db, run: i64, path: &str) -> i64 {
        db.upsert_file(
            &files::NewFile {
                path: path.into(),
                name: pc_core::base_name(path).into(),
                disk: "root".into(),
                size: 1000,
                ..Default::default()
            },
            run,
        )
        .unwrap()
    }

    #[test]
    fn a_reset_clears_the_index_but_keeps_settings_and_the_journal() {
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run(&["/archive".into()], "test").unwrap();
        let id = a_file(&db, run, "/archive/a.jpg");
        db.conn
            .execute("INSERT INTO manual_keepers(file_id) VALUES(?1)", [id])
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO settings(key,value) VALUES('roots','[\"/archive\"]')",
                [],
            )
            .unwrap();
        db.journal_begin(&model::NewJournalEntry {
            run_id: run,
            op: "move",
            target_id: Some(id),
            src: "/archive/a.jpg",
            dst: Some("/archive/.quarantine/a.jpg"),
            size: 1000,
            file_count: 1,
        })
        .unwrap();

        db.reset_index().unwrap();

        let count = |sql: &str| {
            db.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or(-1)
        };
        assert_eq!(count("SELECT count(*) FROM files"), 0);
        assert_eq!(count("SELECT count(*) FROM manual_keepers"), 0);
        // The only record of what was moved out of the archive survives, or
        // quarantine becomes a one-way trip.
        assert_eq!(count("SELECT count(*) FROM journal"), 1);
        assert_eq!(count("SELECT count(*) FROM settings"), 1);
    }

    #[test]
    fn a_reset_of_an_empty_database_is_not_an_error() {
        let db = Db::open_in_memory().unwrap();
        db.reset_index().unwrap();
        db.reset_index().unwrap();
    }
}

