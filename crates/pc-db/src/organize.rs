//! Rows and updates the reorganisation stage needs.
//!
//! Reorganisation is the one stage that changes where a file *lives* while
//! keeping it in the archive. The index has to follow the file, or every
//! later stage would be reasoning about paths that no longer exist.

use anyhow::Result;
use rusqlite::params;

use crate::{Db, JournalEntry};

/// One indexed photograph, with everything needed to decide where it belongs.
#[derive(Debug, Clone, Default)]
pub struct OrganizeRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub size: i64,
    pub mtime: i64,
    pub dev: i64,
    pub disk: String,
    pub taken_at: Option<i64>,
    /// What the index believed the date came from: `exif`, `filename`, ...
    pub date_source: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub camera_model: Option<String>,
}

impl Db {
    /// Every file still in the archive, oldest known date first.
    ///
    /// Unlike `all_indexed` this does not require a perceptual hash: a file
    /// we failed to decode still has a date and still has to end up
    /// somewhere sensible.
    pub fn organize_rows(&self) -> Result<Vec<OrganizeRow>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path, f.name, f.size, f.mtime, f.dev, f.disk,
                    m.taken_at, m.date_source, m.gps_lat, m.gps_lon, m.camera_model
               FROM files f LEFT JOIN meta m ON m.file_id = f.id
              WHERE f.state = 'present'
              ORDER BY f.id",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(OrganizeRow {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    size: r.get(3)?,
                    mtime: r.get(4)?,
                    dev: r.get(5)?,
                    disk: r.get(6)?,
                    taken_at: r.get(7)?,
                    date_source: r.get(8)?,
                    gps_lat: r.get(9)?,
                    gps_lon: r.get(10)?,
                    camera_model: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Follow a file that moved. The name is stored separately and is what
    /// the pairing of a raw file with its JPEG is built on, so it moves too.
    pub fn set_file_path(&self, file_id: i64, path: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET path = ?1, name = ?2 WHERE id = ?3",
            params![path, name, file_id],
        )?;
        Ok(())
    }

    /// Done entries of one operation in one run, newest first — the order an
    /// undo has to walk them in.
    pub fn journal_by_run_op(&self, run_id: i64, op: &str) -> Result<Vec<JournalEntry>> {
        self.journal_rows(
            "SELECT * FROM journal WHERE run_id = ?1 AND op = ?2 AND status = 'done'
              ORDER BY id DESC",
            &[&run_id, &op],
        )
    }

    /// Roots every run walked — the tops of the archive, which a cleanup of
    /// emptied directories must never take away.
    pub fn all_run_roots(&self) -> Result<Vec<String>> {
        let mut st = self.conn.prepare("SELECT roots FROM runs")?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out: Vec<String> = Vec::new();
        for json in rows {
            if let Ok(roots) = serde_json::from_str::<Vec<String>>(&json) {
                for r in roots {
                    if !out.contains(&r) {
                        out.push(r);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Runs that moved files into the reorganised tree, newest first.
    pub fn organize_runs(&self) -> Result<Vec<(i64, i64, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT run_id, COUNT(*), MAX(applied_at) FROM journal
              WHERE op = 'organize' AND status = 'done'
              GROUP BY run_id ORDER BY run_id DESC",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
