//! SQLite storage: schema, migrations and the queries phase 0 needs.

pub mod files;
pub mod marks;
pub mod model;
pub mod organize;
pub mod review;
pub mod schema;

pub use files::{
    FamilyBadge, FamilyRow, FileInfo, FileRow, Identity, IndexStats, MemberRow, NewFile, NewMeta,
    PlanRow, SeriesMemberRow, SeriesRow, TreeFile,
};
pub use marks::{Mark, MarkScope, Marks};
pub use model::{
    Bundle, BundleState, Catalog, Event, JournalEntry, JournalEvent, JournalStatus, KeeperSource,
    Located, Moved, NewBundle, NewCatalog, NewJournalEntry, QuarantineFound,
};
pub use organize::OrganizeRow;

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub struct Db {
    pub conn: Connection,
}

/// SQLite's own answer to "is the file I hold still the one at my path?".
fn has_moved(conn: &Connection) -> Result<bool> {
    let mut moved: std::ffi::c_int = 0;
    // SAFETY: a live connection handle, the schema name is NUL-terminated,
    // and HAS_MOVED writes one int through the pointer.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            conn.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
            (&raw mut moved).cast(),
        )
    };
    if rc != rusqlite::ffi::SQLITE_OK {
        anyhow::bail!("SQLite cannot say which file it opened (code {rc})");
    }
    Ok(moved != 0)
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
        Self::setup(conn)
    }

    /// [`Db::open`] for a bound data folder ([`pc_core::storage`]): the
    /// database must already exist and be the proven file, and that is
    /// established before SQLite writes anything — no journal mode, no
    /// `-wal`/`-shm`, no migration touches a replacement.
    ///
    /// In order:
    ///
    /// 1. the binding confirms the path, the file and its companions;
    /// 2. SQLite opens the path read-write *without* the right to create it
    ///    and without following a link. Opening writes nothing. If it
    ///    fails, the binding is asked again: a name changed since step 1
    ///    — emptied, or a link put there, which SQLite reports as
    ///    `SQLITE_CANTOPEN_SYMLINK` — is the binding's refusal
    ///    ([`pc_core::storage::NotBound`]) with what was found, not an
    ///    unreadable database (el-5x1uh O1);
    /// 3. SQLite itself is asked whether the file it holds is still the one
    ///    at the path (`SQLITE_FCNTL_HAS_MOVED`), and the binding confirms
    ///    the path once more. Together they say that the object SQLite holds
    ///    is the proven one: for it to be another, the name would have to
    ///    change to the other file and back again within these few calls.
    ///    Ordinary replacements cannot be timed that way; a process of this
    ///    user (or root) doing it on purpose is outside the threat model
    ///    ([`pc_core::storage`]);
    /// 4. only then journal mode, pragmas and migrations.
    ///
    /// A refusal at 1–3 closes the connection having written nothing. The
    /// error says which: refused before SQLite opened anything (1–2), or
    /// opened to be checked and closed again (3).
    pub fn open_bound(path: &Path, binding: &dyn pc_core::storage::StorageBinding) -> Result<Self> {
        use rusqlite::OpenFlags;
        let refused = |why: String| {
            anyhow::Error::new(pc_core::storage::NotBound(why)).context(pc_core::tf!(
                "база {0} не открыта; в ней ничего не записано",
                "the database {0} was not opened; nothing was written to it",
                path.display()
            ))
        };
        let refused_after_open = |why: String| {
            anyhow::Error::new(pc_core::storage::NotBound(why)).context(pc_core::tf!(
                "база {0} открыта для проверки и снова закрыта; в ней ничего не записано",
                "the database {0} was opened to be checked and closed again; nothing was \
                 written to it",
                path.display()
            ))
        };
        binding.check_database().map_err(refused)?;
        let conn = match Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ) {
            Ok(conn) => conn,
            Err(e) => {
                let link = matches!(
                    &e,
                    rusqlite::Error::SqliteFailure(f, _)
                        if f.extended_code == rusqlite::ffi::SQLITE_CANTOPEN_SYMLINK
                );
                let link_found = || {
                    pc_core::tr!(
                        "в момент открытия на пути была символическая ссылка — SQLite по \
                         ссылкам не идёт (SQLITE_CANTOPEN_SYMLINK)",
                        "when SQLite opened it, the path held a symbolic link, which SQLite does \
                         not follow (SQLITE_CANTOPEN_SYMLINK)"
                    )
                    .to_string()
                };
                return Err(match (link, binding.check_database()) {
                    (true, Err(why)) => refused(format!("{}; {why}", link_found())),
                    (true, Ok(())) => refused(link_found()),
                    (false, Err(why)) => refused(why),
                    (false, Ok(())) => anyhow::Error::new(e).context(pc_core::tf!(
                        "не удалось открыть базу {0}",
                        "could not open the database {0}",
                        path.display()
                    )),
                });
            }
        };
        if has_moved(&conn)? {
            return Err(refused_after_open(
                pc_core::tr!(
                    "файл под этим именем сменился, пока база открывалась",
                    "the file at this name changed while the database was being opened"
                )
                .into(),
            ));
        }
        binding.check_database().map_err(refused_after_open)?;
        if conn.is_readonly(rusqlite::MAIN_DB)? {
            anyhow::bail!(pc_core::tf!(
                "база {0} открылась только для чтения",
                "the database {0} opened read-only",
                path.display()
            ));
        }
        Self::setup(conn)
    }

    fn setup(conn: Connection) -> Result<Self> {
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
             DELETE FROM review_choices;
             DELETE FROM review_history;
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
             -- The journal survives a reset, because it is the only record of
             -- what left the archive. Its target ids do not: SQLite hands the
             -- same numbers out again to whatever is indexed next, and a row
             -- pointing at a file it has never seen is worse than a row
             -- pointing at nothing. What the entry moved is written in it by
             -- path.
             UPDATE journal SET target_id = NULL;
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
            manifest: &[],
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
    fn quarantine_belongs_to_whoever_the_journal_says_put_it_there() {
        // A bundle of previews goes in as one directory and one journal row,
        // while a walk finds every file inside it. Orphan quarantine offers
        // to adopt or delete whatever the journal does not claim, so a file
        // inside a claimed directory must not look abandoned — and neither
        // must a sidecar, which travels in the manifest of its photograph.
        let db = Db::open_in_memory().unwrap();
        let run = db.start_run(&["/archive".into()], "test").unwrap();
        let q = format!("/archive/{}", pc_core::QUARANTINE_DIR);
        let jid = db
            .journal_begin(&model::NewJournalEntry {
                run_id: run,
                op: "quarantine",
                target_id: None,
                src: "/archive/Library.lrdata",
                dst: Some(&format!("{q}/Library.lrdata")),
                size: 10,
                file_count: 1,
                manifest: &[],
            })
            .unwrap();
        db.journal_finish(jid, model::JournalStatus::Done, None)
            .unwrap();
        let jid = db
            .journal_begin(&model::NewJournalEntry {
                run_id: run,
                op: "quarantine-file",
                target_id: None,
                src: "/archive/frame.arw",
                dst: Some(&format!("{q}/frame.arw")),
                size: 10,
                file_count: 2,
                manifest: &[
                    model::Moved {
                        src: "/archive/frame.arw".into(),
                        dst: format!("{q}/frame.arw"),
                        proof: None,
                    },
                    model::Moved {
                        src: "/archive/frame.xmp".into(),
                        dst: format!("{q}/frame.xmp"),
                        proof: None,
                    },
                ],
            })
            .unwrap();
        db.journal_finish(jid, model::JournalStatus::Done, None)
            .unwrap();

        let seen: Vec<(String, i64, i64)> = [
            format!("{q}/Library.lrdata/sub/cache"),
            format!("{q}/frame.arw"),
            format!("{q}/frame.xmp"),
            format!("{q}/from-an-older-database.jpg"),
        ]
        .iter()
        .map(|p| (p.clone(), 1, 0))
        .collect();
        db.set_quarantine_found(run, &seen).unwrap();

        let found = db.quarantine_found().unwrap();
        let known = |name: &str| {
            found
                .iter()
                .find(|f| f.path.ends_with(name))
                .unwrap_or_else(|| panic!("нет {name}"))
                .known
        };
        assert!(known("cache"), "файл внутри перенесённого каталога — ничей");
        assert!(known("frame.arw"));
        assert!(known("frame.xmp"), "спутник — ничей");
        assert!(!known("from-an-older-database.jpg"), "чужое признано своим");
    }

    #[test]
    fn a_reset_of_an_empty_database_is_not_an_error() {
        let db = Db::open_in_memory().unwrap();
        db.reset_index().unwrap();
        db.reset_index().unwrap();
    }
}

/// The micro-windows of [`Db::open_bound`] (el-5x1uh O1): the name is
/// changed right after the first check, which a test can only reach from
/// inside the binding. Whatever happens, the error is a refusal by the
/// binding (`NotBound`) and says truthfully what SQLite did.
#[cfg(all(test, unix))]
mod open_bound_tests {
    use super::*;
    use pc_core::storage::{NotBound, StorageBinding};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Passes check #1 and then does `after_first` to the database's name;
    /// from then on refuses whatever is not a plain file at the name.
    #[derive(Debug)]
    struct Scripted {
        db: PathBuf,
        calls: AtomicU32,
        after_first: fn(&Path),
        refuse_second: bool,
    }

    impl StorageBinding for Scripted {
        fn check_database(&self) -> std::result::Result<(), String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                (self.after_first)(&self.db);
                return Ok(());
            }
            if self.refuse_second {
                return Err("refused at check 2 (4471)".into());
            }
            match std::fs::symlink_metadata(&self.db) {
                Ok(m) if m.is_file() => Ok(()),
                Ok(_) => Err("not a plain file at the name (4472)".into()),
                Err(e) => Err(format!("nothing at the name: {e}")),
            }
        }
        fn check_thumbnails(&self) -> std::result::Result<(), String> {
            Ok(())
        }
        fn check_thumbnail_folder(&self, _: &std::fs::File) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        // SQLite's NOFOLLOW refuses a link anywhere on the path; a bound
        // folder is recorded without links, so the test's folder is too.
        let db = tmp.path().canonicalize().unwrap().join("photo-cleanup.db");
        drop(Db::open(&db).unwrap());
        for s in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(tmp.path().join(format!("photo-cleanup.db{s}")));
        }
        (tmp, db)
    }

    fn open(db: &Path, after_first: fn(&Path), refuse_second: bool) -> anyhow::Error {
        let binding = Scripted {
            db: db.to_path_buf(),
            calls: AtomicU32::new(0),
            after_first,
            refuse_second,
        };
        match Db::open_bound(db, &binding) {
            Ok(_) => panic!("opened"),
            Err(e) => e,
        }
    }

    /// The proven file swapped for a link to a copy right after check #1:
    /// SQLite refuses the link (`SQLITE_CANTOPEN_SYMLINK`). That is the
    /// binding's refusal, not "could not open the database", and neither
    /// the copy nor the proven file gets a byte.
    #[test]
    fn a_link_put_at_the_name_after_the_check_is_a_refusal_that_names_the_link() {
        let (tmp, db) = fixture();
        fn swap(db: &Path) {
            let dir = db.parent().unwrap();
            std::fs::rename(db, dir.join("saved-own.db")).unwrap();
            std::fs::copy(dir.join("saved-own.db"), dir.join("copy.db")).unwrap();
            std::os::unix::fs::symlink(dir.join("copy.db"), db).unwrap();
        }
        let err = open(&db, swap, false);
        let text = format!("{err:#}");
        assert!(err.downcast_ref::<NotBound>().is_some(), "{text}");
        assert!(text.contains("symbolic link"), "{text}");
        assert!(text.contains("was not opened"), "{text}");
        assert!(text.contains("nothing was written"), "{text}");
        assert!(text.contains("4472"), "the binding's own reason: {text}");
        let own = std::fs::read(tmp.path().join("saved-own.db")).unwrap();
        assert_eq!(std::fs::read(tmp.path().join("copy.db")).unwrap(), own);
        assert!(!tmp.path().join("copy.db-wal").exists());
        assert!(!tmp.path().join("saved-own.db-wal").exists());
    }

    /// The name emptied right after check #1: SQLite may not create it, the
    /// open fails, and check #2 says why — a refusal again.
    #[test]
    fn a_name_emptied_after_the_check_is_a_refusal_and_nothing_is_created() {
        let (tmp, db) = fixture();
        fn away(db: &Path) {
            std::fs::rename(db, db.with_file_name("saved-own.db")).unwrap();
        }
        let err = open(&db, away, false);
        let text = format!("{err:#}");
        assert!(err.downcast_ref::<NotBound>().is_some(), "{text}");
        assert!(text.contains("nothing at the name"), "{text}");
        assert!(!db.exists(), "a database was created at the name");
        assert!(tmp.path().join("saved-own.db").exists());
    }

    /// A refusal after SQLite opened the file (check #2) does not claim the
    /// file was never opened: SQLite opened it and read its header.
    #[test]
    fn a_refusal_after_the_open_says_the_file_was_opened_and_closed() {
        let (tmp, db) = fixture();
        let before = std::fs::read(&db).unwrap();
        let err = open(&db, |_| {}, true);
        let text = format!("{err:#}");
        assert!(err.downcast_ref::<NotBound>().is_some(), "{text}");
        assert!(text.contains("4471"), "{text}");
        assert!(!text.contains("was not opened"), "{text}");
        assert!(text.contains("closed again; nothing was written"), "{text}");
        assert_eq!(std::fs::read(&db).unwrap(), before);
        assert!(!tmp.path().join("photo-cleanup.db-wal").exists());
    }
}
