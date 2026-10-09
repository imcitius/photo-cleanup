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

/// Who chose the file a group keeps.
///
/// Both kinds sit in `manual_keepers`, because both outrank what the tool
/// worked out on its own. They differ in what may undo them: a rule is undone
/// by taking the rule back, a person's answer only by that person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeeperSource {
    /// Pressed on one group, on the groups screen.
    Hand,
    /// Follows from a folder named as holding the originals.
    Folder,
}

impl KeeperSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hand => "hand",
            Self::Folder => "folder",
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
    /// Why this bundle stays where it is — always something.
    ///
    /// Read from the kind and the path, not from what a scan wrote: a
    /// database scanned by an earlier version holds bundles with no block
    /// recorded.
    pub fn protected(&self) -> BlockReason {
        pc_core::derived::refusal(self.kind, std::path::Path::new(&self.path))
    }

    /// What keeps this bundle where it is, in words.
    pub fn refusal(&self) -> String {
        self.protected().describe()
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
    /// Every file this one operation moves, `src` included, fixed before the
    /// first rename. Empty means "the entry names its own path and nothing
    /// else" — a directory moved whole, or a row from an older version.
    pub manifest: &'a [Moved],
}

/// One file's journey inside an operation.
///
/// Read strictly: a key this version does not know is evidence it cannot
/// read, and the whole record is then refused rather than read as a row
/// without evidence (el-1y8uo B2; see [`JournalEntry::manifest_unreadable`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Moved {
    pub src: String,
    pub dst: String,
    /// Which object this is, written before the first rename while it was
    /// certainly the file the operation meant (el-usdqi, el-5vue3). Every
    /// recovery — undo, its retry, the reconciliation of an interrupted run,
    /// the web's preview of either — asks it before treating a file at
    /// either path as this one. Absent in rows written before; such a row
    /// proves nothing about what is at home, and is not trusted to.
    ///
    /// A development build of el-usdqi wrote a bare `ident` string here.
    /// Such a record is not read as a row without evidence: it is evidence
    /// in a form this version does not understand, and fails closed like
    /// any other (el-1y8uo B2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<pc_core::proof::Proof>,
}

impl Moved {
    pub fn new(src: impl Into<String>, dst: impl Into<String>) -> Self {
        Self {
            src: src.into(),
            dst: dst.into(),
            proof: None,
        }
    }
}

/// Where one object of an entry was found, proven or not, after the
/// operation's last rename of it (el-3wizg, diagnosis el-lvtmk R5).
///
/// Appended to an event, never written over the manifest: the manifest says
/// what the operation meant and recorded before it moved anything; this says
/// where an object actually ended up when that differs from the record or
/// could not be proven. Recovery reads the latest one per item over the
/// manifest ([`JournalEntry::located`]) and still decides ownership only by
/// the evidence ([`Moved::proof`]) — a path here is a place to look, never a
/// reason to act.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Located {
    /// The manifest item this concerns, as recorded.
    pub src: String,
    pub dst: String,
    /// `checked` (the object the operation verified), `changed` (that
    /// object, altered since its check) or `stranger` (another object that
    /// took its name). Only the first two are ever recovered.
    pub role: String,
    /// The object is on the operation's side of the move: where `dst` was
    /// meant to be, not back at `src`. Only such an object is overlaid on
    /// `dst` for recovery.
    pub held: bool,
    pub at: pc_core::whereabouts::Whereabouts,
    /// The evidence of the object found, when it could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<pc_core::proof::Proof>,
}

impl Located {
    /// Recovery may look for this item here: the operation's own object,
    /// on its side, at a path proven or last seen.
    pub fn overlay(&self) -> Option<&std::path::Path> {
        (self.held && matches!(self.role.as_str(), "checked" | "changed"))
            .then(|| self.at.hint())
            .flatten()
    }
}

/// One file of an undo whose original place was taken, returned under a
/// free name beside it instead (`IMG.CR2` as `IMG_1.CR2`, el-14vx0).
///
/// Written to the entry's history *before* the rename, so a retry after an
/// interruption knows where to look for what already came back — and still
/// treats a file there as this one only by its evidence ([`Moved::proof`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReturnedAs {
    /// The manifest item this concerns, as recorded.
    pub src: String,
    pub dst: String,
    /// The free name it is returned to instead of `src`.
    pub to: String,
}

/// The decision taken for an undo whose original place was taken
/// (el-14vx0), as appended to the entry's history: which choice, what was
/// returning, what occupied its place — with the evidence read when the
/// choice was offered — and what was done about it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConflictNote {
    /// `keep` or `rename-returning`; empty when there was no choice to make
    /// (the place was free, or the unit could not come back at all).
    pub choice: String,
    /// The unit coming back: `src` its place, `dst` where it is held.
    pub returning: Vec<Moved>,
    /// What bore those places: `src` the path, `proof` the evidence read;
    /// `dst` is always empty — the existing file is never moved.
    pub occupants: Vec<Moved>,
    /// An attempt to return the unit under free names, recorded before it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returned_as: Vec<ReturnedAs>,
    /// How the decision ended, on the event that says so: `kept`,
    /// `renamed-returning`, `refused` or `changed-since-preview`. Absent on
    /// the progress event written before a move (`attempt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// One event in the history of a journal entry. Appended, never replaced:
/// a retry adds to what the entry says, it does not erase it (el-5vue3 R3/D6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEvent {
    pub id: i64,
    pub journal_id: i64,
    pub at: i64,
    /// `forward`, `undo`, `reconcile`, `purge`, `note`.
    pub phase: String,
    /// `done`, `refused`, `partial`, `recovered`, `note`.
    pub kind: String,
    /// The words, as shown.
    pub text: String,
    /// Structured detail (paths, causes, what moved), as JSON.
    pub data: Option<String>,
}

/// One typed outcome to append to an entry's history (el-1y8uo B6): what
/// happened, in which phase, and in structured form — whether or not there
/// are any words for a person. `text` may be empty; the readable note then
/// stays as it was, the event is written all the same.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    /// `forward`, `undo`, `reconcile`, `adopt`, `purge`, `abandon`.
    pub phase: &'a str,
    /// `done`, `partial`, `refused`, `begun`, `kept`.
    pub kind: &'a str,
    /// The words, as shown; may be empty.
    pub text: &'a str,
    /// What this outcome actually moved.
    pub moved: &'a [Moved],
    /// `(path, why)` for every file it did not move.
    pub refused: &'a [(String, String)],
    /// The error that ended it, if one did.
    pub error: Option<&'a str>,
    /// Objects found away from their record, or whose place could not be
    /// proven; see [`Located`].
    pub located: &'a [Located],
}

impl<'a> Event<'a> {
    pub fn new(phase: &'a str, kind: &'a str) -> Self {
        Self {
            phase,
            kind,
            text: "",
            moved: &[],
            refused: &[],
            error: None,
            located: &[],
        }
    }

    fn data(&self, status: JournalStatus) -> String {
        let mut v = serde_json::json!({
            "status": status.as_str(),
            "moved": self
                .moved
                .iter()
                .map(|m| serde_json::json!({"src": m.src, "dst": m.dst}))
                .collect::<Vec<_>>(),
            "refused": self
                .refused
                .iter()
                .map(|(path, why)| serde_json::json!({"path": path, "why": why}))
                .collect::<Vec<_>>(),
            "error": self.error,
        });
        if !self.located.is_empty() {
            v["located"] = serde_json::json!(self.located);
        }
        v.to_string()
    }
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
    /// What actually moved, when the operation wrote it down.
    pub manifest: Vec<Moved>,
    /// The stored list, exactly as written, when it is there but this
    /// version cannot read it — a later version's evidence, or damage.
    /// `manifest` is then empty, and that emptiness must not be taken for a
    /// row from before lists were kept: every recovery and purge refuses
    /// such a row and leaves the record as it is (el-1y8uo B2).
    pub manifest_unreadable: Option<String>,
    /// Every [`Located`] its events carry, oldest first. The latest one per
    /// manifest item is the one that counts.
    pub located: Vec<Located>,
    /// The run the entry was written in.
    pub run_id: i64,
    /// Every attempt to return this entry under free names
    /// ([`ConflictNote::returned_as`]), oldest first.
    pub returned_as: Vec<Vec<ReturnedAs>>,
}

impl JournalEntry {
    /// The latest place recovery may look for the item `m` of this entry.
    pub fn overlay_of(&self, m: &Moved) -> Option<&Located> {
        self.located
            .iter()
            .rev()
            .find(|l| l.src == m.src && l.dst == m.dst && l.role != "stranger")
    }
}

#[derive(Debug, Clone, Default)]
pub struct BundleFilter {
    pub kind: Option<DerivedKind>,
    pub state: Option<BundleState>,
    pub min_size: Option<i64>,
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
            "INSERT INTO journal(run_id, target_id, op, src, dst, size,
                                 file_count, status, applied_at, manifest)
             VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?9)",
            params![
                e.run_id,
                e.target_id,
                e.op,
                e.src,
                e.dst,
                e.size,
                e.file_count,
                pc_core::time::now_unix(),
                manifest_json(e.manifest)
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Write down the list an undo works from, without touching the count
    /// of files the operation moved: a directory moved whole is one entry in
    /// the list and many files in the count.
    pub fn journal_record_manifest(&self, id: i64, moved: &[Moved]) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET manifest=?1 WHERE id=?2",
            params![manifest_json(moved), id],
        )?;
        Ok(())
    }

    /// What the journal says about an entry so far: the note an older
    /// version wrote, followed by every event since, in order.
    pub fn journal_note(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT note FROM journal WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .flatten())
    }

    /// Set the status of an entry and, if given, add `note` to its history.
    ///
    /// It used to replace the note: a retried undo or reconciliation
    /// overwrote what the first attempt had written down (el-5vue3 R3/D6).
    /// Now nothing already written is ever replaced — the note is appended
    /// as an event, and the readable note grows by it.
    ///
    /// For rows written outside an operation — test fixtures, imports. Every
    /// operation of the tool closes its rows with [`Db::journal_close`],
    /// which always records a typed outcome (el-1y8uo B6).
    pub fn journal_finish(&self, id: i64, status: JournalStatus, note: Option<&str>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE journal SET status=?1 WHERE id=?2",
            params![status.as_str(), id],
        )?;
        if let Some(note) = note {
            append_event(&tx, id, "note", "note", note, None)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Set the status of an entry and append the typed outcome that set it,
    /// in one transaction: no state without its event, no event without its
    /// state (el-1y8uo B3/B6). `undone` and `purged` carry their time.
    pub fn journal_close(&self, id: i64, status: JournalStatus, ev: &Event<'_>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE journal SET status=?1,
                    undone_at = CASE WHEN ?1 = 'undone' THEN ?2 ELSE undone_at END,
                    purged_at = CASE WHEN ?1 = 'purged' THEN ?2 ELSE purged_at END
              WHERE id=?3",
            params![status.as_str(), pc_core::time::now_unix(), id],
        )?;
        append_event(&tx, id, ev.phase, ev.kind, ev.text, Some(&ev.data(status)))?;
        tx.commit()?;
        Ok(())
    }

    /// Add one event to an entry's history: kept in `journal_events`, and
    /// appended to the readable note in the same transaction.
    pub fn journal_event(
        &self,
        id: i64,
        phase: &str,
        kind: &str,
        text: &str,
        data: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        append_event(&tx, id, phase, kind, text, data)?;
        tx.commit()?;
        Ok(())
    }

    /// [`Db::journal_event`] with the places of what it concerns
    /// ([`Located`]), read back into [`JournalEntry::located`].
    pub fn journal_event_located(
        &self,
        id: i64,
        phase: &str,
        kind: &str,
        text: &str,
        located: &[Located],
    ) -> Result<()> {
        let data =
            (!located.is_empty()).then(|| serde_json::json!({ "located": located }).to_string());
        self.journal_event(id, phase, kind, text, data.as_deref())
    }

    /// [`Db::journal_event`] with the decision on an undo whose place was
    /// taken ([`ConflictNote`]); an attempt under free names, written as an
    /// `attempt` event, is read back into [`JournalEntry::returned_as`] —
    /// the outcome event that repeats it once it came back is not another
    /// attempt.
    pub fn journal_event_conflict(
        &self,
        id: i64,
        phase: &str,
        kind: &str,
        text: &str,
        note: &ConflictNote,
    ) -> Result<()> {
        let data = serde_json::json!({ "conflict": note }).to_string();
        self.journal_event(id, phase, kind, text, Some(&data))
    }

    /// An entry's events, oldest first.
    pub fn journal_events(&self, id: i64) -> Result<Vec<JournalEvent>> {
        let mut st = self.conn.prepare(
            "SELECT id, journal_id, at, phase, kind, text, data FROM journal_events
              WHERE journal_id = ?1 ORDER BY id",
        )?;
        let rows = st
            .query_map([id], |r| {
                Ok(JournalEvent {
                    id: r.get(0)?,
                    journal_id: r.get(1)?,
                    at: r.get(2)?,
                    phase: r.get(3)?,
                    kind: r.get(4)?,
                    text: r.get(5)?,
                    data: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// What an operation actually moved, before it is finished: the list,
    /// the number of files, and their bytes — not what it planned (el-5vue3
    /// D7). A directory moved whole is one entry and `file_count` files.
    pub fn journal_finalize(
        &self,
        id: i64,
        moved: &[Moved],
        file_count: i64,
        size: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE journal SET manifest=?1, file_count=?2, size=?3 WHERE id=?4",
            params![manifest_json(moved), file_count, size, id],
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
                let raw: Option<String> = r.get("manifest")?;
                let (manifest, manifest_unreadable) = match raw {
                    None => (Vec::new(), None),
                    Some(j) if j.trim().is_empty() => (Vec::new(), None),
                    Some(j) => match serde_json::from_str::<Vec<Moved>>(&j) {
                        Ok(m) => (m, None),
                        Err(_) => (Vec::new(), Some(j)),
                    },
                };
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
                    manifest,
                    manifest_unreadable,
                    located: Vec::new(),
                    run_id: r.get("run_id")?,
                    returned_as: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut rows = rows;
        let mut st = self.conn.prepare_cached(
            "SELECT data FROM journal_events
              WHERE journal_id = ?1 AND instr(data, '\"located\"') > 0 ORDER BY id",
        )?;
        for row in &mut rows {
            let datas = st
                .query_map([row.id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for d in datas {
                row.located.extend(located_of(&d)?);
            }
        }
        let mut st = self.conn.prepare_cached(
            "SELECT data FROM journal_events
              WHERE journal_id = ?1 AND kind = 'attempt'
                AND instr(data, '\"returned_as\"') > 0 ORDER BY id",
        )?;
        for row in &mut rows {
            let datas = st
                .query_map([row.id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for d in datas {
                let as_ = returned_as_of(&d)?;
                if !as_.is_empty() {
                    row.returned_as.push(as_);
                }
            }
        }
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

    /// Finished entries with an object of their own found away from its
    /// recorded place, or whose place could not be proven (el-lvtmk R6):
    /// what `status` lists and undo can still act on. A refused entry never
    /// keeps anything of its own (user decision (c)); one that could not put
    /// a photograph back stays pending and is listed with those.
    pub fn journal_located(&self) -> Result<Vec<JournalEntry>> {
        Ok(self
            .journal_rows(
                "SELECT * FROM journal WHERE status = 'done' AND id IN
                   (SELECT journal_id FROM journal_events WHERE instr(data, '\"located\"') > 0)
                 ORDER BY id",
                &[],
            )?
            .into_iter()
            .filter(|e| e.located.iter().any(|l| l.overlay().is_some()))
            .collect())
    }

    pub fn journal_pending(&self) -> Result<Vec<JournalEntry>> {
        self.journal_rows(
            "SELECT * FROM journal WHERE status='pending' ORDER BY id",
            &[],
        )
    }
}

/// Files found sitting in our quarantine folders during a walk.
#[derive(Debug, Clone)]
pub struct QuarantineFound {
    pub path: String,
    pub size: i64,
    pub mtime: i64,
    /// Whether the journal knows how to put this one back.
    pub known: bool,
}

impl Db {
    /// Replace what the last walk saw. The folders are the truth here; the
    /// table is only a note of what was stepped over.
    pub fn set_quarantine_found(&self, run_id: i64, found: &[(String, i64, i64)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.conn.execute("DELETE FROM quarantine_found", [])?;
        {
            let mut st = self.conn.prepare(
                "INSERT OR REPLACE INTO quarantine_found(path, size, mtime, seen_run)
                 VALUES(?1, ?2, ?3, ?4)",
            )?;
            for (path, size, mtime) in found {
                st.execute(rusqlite::params![path, size, mtime, run_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// A file that has just been dealt with is no longer news. The table is
    /// refreshed by a walk, and waiting for one would leave the screen
    /// claiming files that are no longer there.
    pub fn forget_quarantine_found(&self, path: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM quarantine_found WHERE path = ?1", [path])?;
        Ok(())
    }

    /// What is in quarantine on disk, and whether the journal can undo it.
    ///
    /// A file the journal does not know about was put there by a database
    /// that is no longer here. It is not rubbish and not indexed either: it
    /// simply belongs to nobody until someone says what to do with it.
    pub fn quarantine_found(&self) -> Result<Vec<QuarantineFound>> {
        // Everything the journal put there, sidecars included: a `.xmp` that
        // travelled with its photograph has no journal row of its own, and
        // calling it nobody's would offer it to be adopted or purged apart
        // from the frame it belongs to.
        let mut ours: std::collections::HashSet<String> = std::collections::HashSet::new();
        {
            // `pending` counts too: an operation that was interrupted owns
            // what it was moving, and calling that nobody's would offer to
            // adopt a file its own journal row is still waiting to explain.
            let mut st = self
                .conn
                .prepare("SELECT dst, manifest FROM journal WHERE status IN ('done', 'pending')")?;
            let mut rows = st.query([])?;
            while let Some(r) = rows.next()? {
                if let Some(dst) = r.get::<_, Option<String>>(0)? {
                    ours.insert(dst);
                }
                if let Some(json) = r.get::<_, Option<String>>(1)? {
                    // Only the paths, read leniently: a list whose evidence
                    // this version cannot read still claims what it moved.
                    if let Ok(serde_json::Value::Array(moved)) = serde_json::from_str(&json) {
                        ours.extend(
                            moved
                                .iter()
                                .filter_map(|m| m.get("dst")?.as_str().map(String::from)),
                        );
                    }
                }
            }
        }
        let mut st = self
            .conn
            .prepare("SELECT path, size, mtime FROM quarantine_found ORDER BY path")?;
        let rows = st
            .query_map([], |r| {
                let path: String = r.get(0)?;
                Ok(QuarantineFound {
                    known: claimed(&ours, &path),
                    path,
                    size: r.get(1)?,
                    mtime: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

/// Whether the journal put this path in quarantine — itself, or as part of a
/// directory it moved whole.
///
/// A bundle of Lightroom previews goes into quarantine as one directory and
/// one journal row, while a walk finds every file inside it. Matching the
/// path alone would call all of them abandoned. Walking up the path costs a
/// few lookups; asking the journal about every file would cost the journal.
fn claimed(ours: &std::collections::HashSet<String>, path: &str) -> bool {
    if ours.contains(path) {
        return true;
    }
    let mut cut = path;
    while let Some(i) = cut.rfind(if cfg!(windows) {
        &['/', '\\'][..]
    } else {
        &['/'][..]
    }) {
        cut = &cut[..i];
        if cut.is_empty() {
            break;
        }
        if ours.contains(cut) {
            return true;
        }
    }
    false
}

/// The `located` list of one event's data. A list this version cannot
/// read is an error, not an empty list: an unread place would let purge or
/// recovery act as if nothing had been found elsewhere (el-1y8uo B2).
fn located_of(data: &str) -> Result<Vec<Located>> {
    let v: serde_json::Value = serde_json::from_str(data)?;
    match v.get("located") {
        None => Ok(Vec::new()),
        Some(l) => Ok(serde_json::from_value(l.clone()).map_err(|e| {
            anyhow::anyhow!("a located record this version cannot read ({e}): {l}")
        })?),
    }
}

fn returned_as_of(data: &str) -> Result<Vec<ReturnedAs>> {
    let v: serde_json::Value = serde_json::from_str(data)?;
    match v.get("conflict") {
        None => Ok(Vec::new()),
        Some(c) => Ok(serde_json::from_value::<ConflictNote>(c.clone())
            .map_err(|e| anyhow::anyhow!("a conflict record this version cannot read ({e}): {c}"))?
            .returned_as),
    }
}

fn append_event(
    conn: &rusqlite::Connection,
    id: i64,
    phase: &str,
    kind: &str,
    text: &str,
    data: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO journal_events(journal_id, at, phase, kind, text, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, pc_core::time::now_unix(), phase, kind, text, data],
    )?;
    // The readable note grows; what it said before stays its beginning. An
    // event without words leaves it as it is.
    if text.is_empty() {
        return Ok(());
    }
    conn.execute(
        "UPDATE journal SET note = CASE WHEN note IS NULL OR note = '' THEN ?1
                                        ELSE note || ' | ' || ?1 END
          WHERE id = ?2",
        params![text, id],
    )?;
    Ok(())
}

/// A manifest is stored as JSON, and an empty one as nothing at all: a row
/// with no list is exactly a row from before this was written down.
fn manifest_json(moved: &[Moved]) -> Option<String> {
    (!moved.is_empty()).then(|| serde_json::to_string(moved).unwrap_or_default())
}
