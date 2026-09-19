use anyhow::Result;
use pc_core::{BlockReason, DerivedKind};
use rusqlite::{params, OptionalExtension, Row};

use crate::Db;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleState {
    Present,
    Quarantined,
    Purged,
}

impl BundleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Quarantined => "quarantined",
            Self::Purged => "purged",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "quarantined" => Self::Quarantined,
            "purged" => Self::Purged,
            _ => Self::Present,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalStatus {
    Pending,
    Done,
    Failed,
    Undone,
    Purged,
}

impl JournalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Undone => "undone",
            Self::Purged => "purged",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "done" => Self::Done,
            "failed" => Self::Failed,
            "undone" => Self::Undone,
            "purged" => Self::Purged,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewBundle {
    pub path: String,
    pub is_dir: bool,
    pub disk: String,
    pub dev: i64,
    pub mount: String,
    pub kind: DerivedKind,
    pub owner_ref: Option<String>,
    pub file_count: i64,
    pub size: i64,
    pub newest_mtime: i64,
}

#[derive(Debug, Clone)]
pub struct Bundle {
    pub id: i64,
    pub path: String,
    pub is_dir: bool,
    pub disk: String,
    pub dev: i64,
    pub mount: String,
    pub kind: DerivedKind,
    pub owner_ref: Option<String>,
    pub file_count: i64,
    pub size: i64,
    pub newest_mtime: i64,
    pub regenerable: bool,
    pub blocked_code: Option<String>,
    pub blocked_detail: Option<String>,
    pub rebuild_cost_hint: Option<String>,
    pub state: BundleState,
}

impl Bundle {
    /// Selectable means: the kind can be rebuilt and no gate blocks it.
    pub fn removable(&self) -> bool {
        self.regenerable && self.blocked_code.is_none() && self.state == BundleState::Present
    }

    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let kind_s: String = r.get("kind")?;
        let state_s: String = r.get("state")?;
        Ok(Self {
            id: r.get("id")?,
            path: r.get("path")?,
            is_dir: r.get::<_, i64>("is_dir")? != 0,
            disk: r.get("disk")?,
            dev: r.get("dev")?,
            mount: r.get("mount")?,
            kind: DerivedKind::parse(&kind_s).unwrap_or(DerivedKind::LrDataOther),
            owner_ref: r.get("owner_ref")?,
            file_count: r.get("file_count")?,
            size: r.get("size")?,
            newest_mtime: r.get("newest_mtime")?,
            regenerable: r.get::<_, i64>("regenerable")? != 0,
            blocked_code: r.get("blocked_code")?,
            blocked_detail: r.get("blocked_detail")?,
            rebuild_cost_hint: r.get("rebuild_cost_hint")?,
            state: BundleState::parse(&state_s),
        })
    }
}

#[derive(Debug, Clone)]
pub struct NewCatalog {
    pub path: String,
    pub name: String,
    pub disk: String,
    pub size: i64,
    pub is_backup: bool,
    pub is_locked: bool,
    pub image_count: Option<i64>,
    pub read_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub is_backup: bool,
    pub is_locked: bool,
    pub image_count: Option<i64>,
    pub read_error: Option<String>,
}

/// Arguments for opening a journal record, grouped so the call site reads as
/// a description of the action rather than a row of positional values.
#[derive(Debug, Clone)]
pub struct NewJournalEntry<'a> {
    pub run_id: i64,
    pub op: &'a str,
    pub target_id: Option<i64>,
    pub src: &'a str,
    pub dst: Option<&'a str>,
    pub size: i64,
    pub file_count: i64,
}

#[derive(Debug, Clone)]
pub struct JournalEntry {
    pub id: i64,
    pub op: String,
    pub src: String,
    pub dst: Option<String>,
    pub size: i64,
    pub file_count: i64,
    pub status: JournalStatus,
    pub applied_at: i64,
    pub target_id: Option<i64>,
}

#[derive(Debug, Clone, Default)]
pub struct BundleFilter {
    pub kind: Option<DerivedKind>,
    pub state: Option<BundleState>,
    pub min_size: Option<i64>,
    /// Only bundles that may actually be removed.
    pub removable_only: bool,
}

impl Db {
    // ---- catalogs ---------------------------------------------------------

    pub fn upsert_catalog(&self, c: &NewCatalog) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO lr_catalogs(path, name, disk, size, is_backup, is_locked,
                                     image_count, read_error, indexed_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(path) DO UPDATE SET
                 name=excluded.name, disk=excluded.disk, size=excluded.size,
                 is_backup=excluded.is_backup, is_locked=excluded.is_locked,
                 image_count=excluded.image_count, read_error=excluded.read_error,
                 indexed_at=excluded.indexed_at",
            params![
                c.path,
                c.name,
                c.disk,
                c.size,
                c.is_backup as i64,
                c.is_locked as i64,
                c.image_count,
                c.read_error,
                pc_core::time::now_unix()
            ],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM lr_catalogs WHERE path = ?1",
            params![c.path],
            |r| r.get(0),
        )?)
    }

    pub fn catalog_by_path(&self, path: &str) -> Result<Option<Catalog>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, path, name, is_backup, is_locked, image_count, read_error
                 FROM lr_catalogs WHERE path = ?1",
                params![path],
                |r| {
                    Ok(Catalog {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        name: r.get(2)?,
                        is_backup: r.get::<_, i64>(3)? != 0,
                        is_locked: r.get::<_, i64>(4)? != 0,
                        image_count: r.get(5)?,
                        read_error: r.get(6)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn all_catalogs(&self) -> Result<Vec<Catalog>> {
        let mut st = self.conn.prepare(
            "SELECT id, path, name, is_backup, is_locked, image_count, read_error
             FROM lr_catalogs ORDER BY path",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(Catalog {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    is_backup: r.get::<_, i64>(3)? != 0,
                    is_locked: r.get::<_, i64>(4)? != 0,
                    image_count: r.get(5)?,
                    read_error: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- bundles ----------------------------------------------------------

    pub fn upsert_bundle(&self, b: &NewBundle, run_id: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO derived_bundles(path, is_dir, disk, dev, mount, kind, owner_ref,
                                         file_count, size, newest_mtime, regenerable,
                                         scanned_run, state)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'present')
             ON CONFLICT(path) DO UPDATE SET
                 is_dir=excluded.is_dir, disk=excluded.disk, dev=excluded.dev,
                 mount=excluded.mount, kind=excluded.kind, owner_ref=excluded.owner_ref,
                 file_count=excluded.file_count, size=excluded.size,
                 newest_mtime=excluded.newest_mtime, regenerable=excluded.regenerable,
                 scanned_run=excluded.scanned_run,
                 -- a rescan that finds the bundle back on disk clears a stale state
                 state='present'",
            params![
                b.path,
                b.is_dir as i64,
                b.disk,
                b.dev,
                b.mount,
                b.kind.as_str(),
                b.owner_ref,
                b.file_count,
                b.size,
                b.newest_mtime,
                b.kind.regenerable() as i64,
                run_id
            ],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM derived_bundles WHERE path = ?1",
            params![b.path],
            |r| r.get(0),
        )?)
    }

    pub fn set_block(&self, bundle_id: i64, reason: Option<&BlockReason>) -> Result<()> {
        match reason {
            Some(r) => self.conn.execute(
                "UPDATE derived_bundles SET blocked_code=?1, blocked_detail=?2 WHERE id=?3",
                params![r.code(), r.describe(), bundle_id],
            )?,
            None => self.conn.execute(
                "UPDATE derived_bundles SET blocked_code=NULL, blocked_detail=NULL WHERE id=?1",
                params![bundle_id],
            )?,
        };
        Ok(())
    }

    pub fn set_rebuild_hint(&self, bundle_id: i64, hint: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE derived_bundles SET rebuild_cost_hint=?1 WHERE id=?2",
            params![hint, bundle_id],
        )?;
        Ok(())
    }

    pub fn set_bundle_state(&self, bundle_id: i64, state: BundleState) -> Result<()> {
        self.conn.execute(
            "UPDATE derived_bundles SET state=?1 WHERE id=?2",
            params![state.as_str(), bundle_id],
        )?;
        Ok(())
    }

    pub fn bundle(&self, id: i64) -> Result<Option<Bundle>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM derived_bundles WHERE id = ?1",
                params![id],
                Bundle::from_row,
            )
            .optional()?)
    }

    pub fn list_bundles(&self, f: &BundleFilter) -> Result<Vec<Bundle>> {
        let mut sql = String::from("SELECT * FROM derived_bundles WHERE 1=1");
        if f.kind.is_some() {
            sql.push_str(" AND kind = :kind");
        }
        if f.state.is_some() {
            sql.push_str(" AND state = :state");
        }
        if f.min_size.is_some() {
            sql.push_str(" AND size >= :min_size");
        }
        if f.removable_only {
            sql.push_str(" AND regenerable = 1 AND blocked_code IS NULL AND state = 'present'");
        }
        sql.push_str(" ORDER BY size DESC, path");

        let mut st = self.conn.prepare(&sql)?;
        let mut named: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        let kind_s = f.kind.map(|k| k.as_str());
        let state_s = f.state.map(|s| s.as_str());
        if let Some(k) = &kind_s {
            named.push((":kind", k));
        }
        if let Some(s) = &state_s {
            named.push((":state", s));
        }
        if let Some(m) = &f.min_size {
            named.push((":min_size", m));
        }
        let rows = st
            .query_map(named.as_slice(), Bundle::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Bundles owned by a given catalog path.
    pub fn bundles_of_owner(&self, owner: &str) -> Result<Vec<Bundle>> {
        let mut st = self
            .conn
            .prepare("SELECT * FROM derived_bundles WHERE owner_ref = ?1")?;
        let rows = st
            .query_map(params![owner], Bundle::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- journal ----------------------------------------------------------

    pub fn journal_begin(&self, e: &NewJournalEntry<'_>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO journal(run_id, target_kind, target_id, op, src, dst, size,
                                 file_count, status, applied_at)
             VALUES (?1,'derived-bundle',?2,?3,?4,?5,?6,?7,'pending',?8)",
            params![
                e.run_id,
                e.target_id,
                e.op,
                e.src,
                e.dst,
                e.size,
                e.file_count,
                pc_core::time::now_unix()
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn journal_finish(&self, id: i64, status: JournalStatus, note: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET status=?1, note=?2 WHERE id=?3",
            params![status.as_str(), note, id],
        )?;
        Ok(())
    }

    pub fn journal_mark_undone(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET status='undone', undone_at=?1 WHERE id=?2",
            params![pc_core::time::now_unix(), id],
        )?;
        Ok(())
    }

    pub fn journal_mark_purged(&self, id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET status='purged', purged_at=?1 WHERE id=?2",
            params![pc_core::time::now_unix(), id],
        )?;
        Ok(())
    }

    pub(crate) fn journal_rows(
        &self,
        sql: &str,
        p: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<JournalEntry>> {
        let mut st = self.conn.prepare(sql)?;
        let rows = st
            .query_map(p, |r| {
                let status: String = r.get("status")?;
                Ok(JournalEntry {
                    id: r.get("id")?,
                    op: r.get("op")?,
                    src: r.get("src")?,
                    dst: r.get("dst")?,
                    size: r.get("size")?,
                    file_count: r.get("file_count")?,
                    status: JournalStatus::parse(&status),
                    applied_at: r.get("applied_at")?,
                    target_id: r.get("target_id")?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn journal_entry(&self, id: i64) -> Result<Option<JournalEntry>> {
        Ok(self
            .journal_rows("SELECT * FROM journal WHERE id = ?1", &[&id])?
            .into_iter()
            .next())
    }

    /// Entries still sitting in quarantine, oldest first.
    ///
    /// Both kinds count: a regenerable bundle and a photograph are moved by
    /// different code paths but land in the same quarantine, and anything
    /// that forgets one of them leaves that space unreclaimable and those
    /// files invisible to `status` and `purge`.
    pub fn journal_quarantined(&self, applied_before: Option<i64>) -> Result<Vec<JournalEntry>> {
        const OPS: &str = "op IN ('quarantine', 'quarantine-file')";
        match applied_before {
            Some(ts) => self.journal_rows(
                &format!(
                    "SELECT * FROM journal WHERE status='done' AND {OPS}
                       AND applied_at <= ?1 ORDER BY applied_at"
                ),
                &[&ts],
            ),
            None => self.journal_rows(
                &format!("SELECT * FROM journal WHERE status='done' AND {OPS} ORDER BY applied_at"),
                &[],
            ),
        }
    }

    pub fn journal_pending(&self) -> Result<Vec<JournalEntry>> {
        self.journal_rows(
            "SELECT * FROM journal WHERE status='pending' ORDER BY id",
            &[],
        )
    }
}
