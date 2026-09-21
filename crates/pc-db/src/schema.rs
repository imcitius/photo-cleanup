use anyhow::Result;
use rusqlite::Connection;

/// Ordered, append-only. Each entry runs once inside its own transaction.
const MIGRATIONS: &[&str] = &[
    // 001 — phase 0: inventory of regenerable derived data, plus the
    // quarantine journal that every later phase reuses.
    r#"
    CREATE TABLE runs(
        id           INTEGER PRIMARY KEY,
        started_at   INTEGER NOT NULL,
        finished_at  INTEGER,
        roots        TEXT    NOT NULL,
        tool_version TEXT    NOT NULL
    );

    CREATE TABLE lr_catalogs(
        id          INTEGER PRIMARY KEY,
        path        TEXT    NOT NULL UNIQUE,
        name        TEXT    NOT NULL,
        disk        TEXT    NOT NULL,
        size        INTEGER NOT NULL,
        is_backup   INTEGER NOT NULL DEFAULT 0,
        is_locked   INTEGER NOT NULL DEFAULT 0,
        image_count INTEGER,
        read_error  TEXT,
        indexed_at  INTEGER NOT NULL
    );

    CREATE TABLE derived_bundles(
        id                INTEGER PRIMARY KEY,
        path              TEXT    NOT NULL UNIQUE,
        is_dir            INTEGER NOT NULL,
        disk              TEXT    NOT NULL,
        dev               INTEGER NOT NULL,
        mount             TEXT    NOT NULL,
        kind              TEXT    NOT NULL,
        owner_ref         TEXT,
        file_count        INTEGER NOT NULL,
        size              INTEGER NOT NULL,
        newest_mtime      INTEGER NOT NULL,
        regenerable       INTEGER NOT NULL,
        blocked_code      TEXT,
        blocked_detail    TEXT,
        rebuild_cost_hint TEXT,
        scanned_run       INTEGER NOT NULL REFERENCES runs(id),
        state             TEXT    NOT NULL DEFAULT 'present'
    );

    CREATE INDEX derived_bundles_kind  ON derived_bundles(kind);
    CREATE INDEX derived_bundles_state ON derived_bundles(state);
    CREATE INDEX derived_bundles_disk  ON derived_bundles(disk);
    CREATE INDEX derived_bundles_owner ON derived_bundles(owner_ref);

    -- Written before the filesystem is touched and updated afterwards, so an
    -- interrupted run leaves a 'pending' row pointing at what to inspect.
    CREATE TABLE journal(
        id          INTEGER PRIMARY KEY,
        run_id      INTEGER NOT NULL REFERENCES runs(id),
        target_kind TEXT    NOT NULL,
        target_id   INTEGER,
        op          TEXT    NOT NULL,
        src         TEXT    NOT NULL,
        dst         TEXT,
        size        INTEGER NOT NULL,
        file_count  INTEGER NOT NULL,
        status      TEXT    NOT NULL,
        applied_at  INTEGER NOT NULL,
        undone_at   INTEGER,
        purged_at   INTEGER,
        note        TEXT
    );

    CREATE INDEX journal_status ON journal(status);
    CREATE INDEX journal_target ON journal(target_kind, target_id);
    "#,
    // 002 — phase 1: the image index itself.
    r#"
    CREATE TABLE files(
        id              INTEGER PRIMARY KEY,
        path            TEXT    NOT NULL UNIQUE,
        name            TEXT    NOT NULL,
        disk            TEXT    NOT NULL,
        dev             INTEGER NOT NULL,
        inode           INTEGER NOT NULL,
        nlink           INTEGER NOT NULL,
        size            INTEGER NOT NULL,
        mtime           INTEGER NOT NULL,
        container       TEXT,
        extension_lied  INTEGER NOT NULL DEFAULT 0,
        width           INTEGER,
        height          INTEGER,
        orientation     INTEGER,
        pixel_source    TEXT,
        -- size plus the head and tail of the file: enough to separate photos
        -- without reading 149 GiB of sensor data. The full hash is computed
        -- only when a file is about to be acted on.
        partial_hash    BLOB,
        full_hash       BLOB,
        pixel_hash      BLOB,
        phash           INTEGER,
        dhash           INTEGER,
        phash_crops     BLOB,
        thumb_key       TEXT,
        skipped_reason  TEXT,
        indexed_run     INTEGER REFERENCES runs(id),
        first_seen_run  INTEGER,
        last_seen_run   INTEGER
    );

    CREATE INDEX files_partial ON files(partial_hash);
    CREATE INDEX files_pixel   ON files(pixel_hash);
    CREATE INDEX files_phash   ON files(phash);
    CREATE INDEX files_size    ON files(size);
    CREATE INDEX files_inode   ON files(dev, inode);
    CREATE INDEX files_disk    ON files(disk);
    CREATE INDEX files_name    ON files(name);

    CREATE TABLE meta(
        file_id         INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
        taken_at        INTEGER,
        date_source     TEXT,
        camera_make     TEXT,
        camera_model    TEXT,
        body_serial     TEXT,
        lens            TEXT,
        iso             INTEGER,
        f_number        REAL,
        focal_length    REAL,
        exposure        TEXT,
        gps_lat         REAL,
        gps_lon         REAL,
        software        TEXT,
        xmp_document_id TEXT,
        xmp_original_id TEXT,
        xmp_derived_from TEXT,
        dng_original_raw TEXT
    );

    CREATE INDEX meta_taken     ON meta(taken_at);
    CREATE INDEX meta_shot      ON meta(body_serial, taken_at);
    CREATE INDEX meta_dng       ON meta(dng_original_raw);
    CREATE INDEX meta_docid     ON meta(xmp_document_id);
    CREATE INDEX meta_derived   ON meta(xmp_derived_from);
    "#,
    // 003 — phase 1: one shutter press per family, one role per file.
    r#"
    CREATE TABLE families(
        id          INTEGER PRIMARY KEY,
        key_kind    TEXT    NOT NULL,
        key_value   TEXT,
        confidence  REAL    NOT NULL DEFAULT 1.0,
        taken_at    INTEGER,
        camera      TEXT,
        keeper_file INTEGER REFERENCES files(id),
        built_run   INTEGER REFERENCES runs(id)
    );

    CREATE TABLE family_members(
        family_id   INTEGER NOT NULL REFERENCES families(id) ON DELETE CASCADE,
        file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        role        TEXT    NOT NULL,
        tier        TEXT,
        evidence    TEXT,
        quality     REAL,
        breakdown   TEXT,
        PRIMARY KEY (family_id, file_id)
    );

    CREATE INDEX family_members_file ON family_members(file_id);
    CREATE INDEX family_members_role ON family_members(role);
    CREATE INDEX families_taken      ON families(taken_at);
    "#,
    // 004 — files a live Lightroom catalog points at.
    //
    // These are the frames the photographer has already curated. They may be
    // the keeper of a family, but they are never a deletion candidate until
    // the protection is lifted deliberately.
    r#"
    CREATE TABLE lr_files(
        catalog_id INTEGER NOT NULL REFERENCES lr_catalogs(id) ON DELETE CASCADE,
        path       TEXT    NOT NULL,
        rating     INTEGER,
        pick       INTEGER,
        PRIMARY KEY (catalog_id, path)
    );

    CREATE INDEX lr_files_path ON lr_files(path);
    "#,
    // 005 — a file that has been moved out of the archive.
    //
    // Without this the planner keeps proposing files it already moved: the
    // rows still say they are on disk, so the same work is offered again and
    // the totals never go down.
    r#"
    ALTER TABLE files ADD COLUMN state TEXT NOT NULL DEFAULT 'present';
    CREATE INDEX files_state ON files(state);
    "#,
    // 006 — phase 3: technical quality, and the series it lets us rank.
    r#"
    ALTER TABLE files ADD COLUMN sharpness REAL;
    ALTER TABLE files ADD COLUMN clip_low  REAL;
    ALTER TABLE files ADD COLUMN clip_high REAL;
    ALTER TABLE files ADD COLUMN entropy   REAL;
    ALTER TABLE files ADD COLUMN contrast  REAL;

    CREATE TABLE series(
        id         INTEGER PRIMARY KEY,
        kind       TEXT    NOT NULL,
        started_at INTEGER,
        camera     TEXT,
        best_file  INTEGER REFERENCES files(id),
        -- Pixel-shift sets are four frames of one scene that a camera merges
        -- later. They look identical and must never be thinned.
        protected  INTEGER NOT NULL DEFAULT 0,
        built_run  INTEGER REFERENCES runs(id)
    );

    CREATE TABLE series_members(
        series_id INTEGER NOT NULL REFERENCES series(id) ON DELETE CASCADE,
        file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        rank      INTEGER NOT NULL,
        score     REAL,
        breakdown TEXT,
        PRIMARY KEY (series_id, file_id)
    );

    CREATE INDEX series_members_file ON series_members(file_id);
    CREATE INDEX series_started      ON series(started_at);
    "#,
    // 007 — phase 4: what kind of picture this is.
    r#"
    ALTER TABLE files ADD COLUMN saturation     REAL;
    ALTER TABLE files ADD COLUMN white_fraction REAL;
    ALTER TABLE files ADD COLUMN bimodality     REAL;
    ALTER TABLE files ADD COLUMN text_rows      REAL;

    CREATE TABLE file_categories(
        file_id    INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
        category   TEXT    NOT NULL,
        confidence REAL    NOT NULL,
        evidence   TEXT,
        -- A verdict the user corrected by hand is never overwritten by a
        -- later pass: their answer is better than the measurement.
        manual     INTEGER NOT NULL DEFAULT 0
    );

    CREATE INDEX file_categories_cat ON file_categories(category);
    "#,
    // 008 — writing comes in lines with gaps; a fence does not.
    r#"
    ALTER TABLE files ADD COLUMN text_banding REAL;
    "#,
    r#"
    CREATE TABLE jobs(id INTEGER PRIMARY KEY, kind TEXT NOT NULL, params TEXT NOT NULL,
      state TEXT NOT NULL, progress TEXT NOT NULL DEFAULT '{}', started_at INTEGER NOT NULL,
      finished_at INTEGER, error TEXT, run_id INTEGER);
    CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE manual_keepers(file_id INTEGER PRIMARY KEY REFERENCES files(id));
    CREATE TABLE manual_splits(file_id INTEGER PRIMARY KEY REFERENCES files(id));
    CREATE TABLE manual_best(file_id INTEGER PRIMARY KEY REFERENCES files(id));
    "#,
    // 010 — frames the user looked at and did not want.
    //
    // The planner reasons about families: a file is a candidate because some
    // other file makes it redundant. A burst of seventy near-identical frames
    // has no such argument to offer — they are all different photographs, and
    // which of them is worth keeping is a judgement only the person who was
    // there can make. This is where that judgement is written down.
    r#"
    CREATE TABLE manual_rejects(
        file_id  INTEGER PRIMARY KEY REFERENCES files(id),
        marked_at INTEGER NOT NULL
    );
    "#,
    // 011 — two measurements the kinds were getting wrong without.
    //
    // `saturation` is absolute chroma, so it falls with the light and a
    // forest at dusk measured the same as a black-and-white print. `chroma`
    // is colour relative to brightness, which a monochrome frame has none of
    // at any exposure. `tonal_range` is the span of the histogram, which
    // tells a lens cap from a moon on a black sky — both are nearly all
    // black, but only one of them has something in it.
    r#"
    ALTER TABLE files ADD COLUMN chroma      REAL;
    ALTER TABLE files ADD COLUMN tonal_range REAL;
    "#,
    // 012 — what is already in quarantine, as the disk has it.
    //
    // Quarantine is a folder beside the photograph and a row in the journal.
    // The folder outlives the database: reset the index, or move it, and the
    // files stay where they were put with nothing left that knows how to
    // bring them back. The walk skips those folders — it must, or the tool
    // would keep re-discovering its own work — so this is where it writes
    // down what it stepped over.
    r#"
    CREATE TABLE quarantine_found(
        path     TEXT PRIMARY KEY,
        size     INTEGER NOT NULL,
        mtime    INTEGER NOT NULL,
        seen_run INTEGER NOT NULL
    );
    "#,
    // 013 — what "the same picture" is allowed to mean, and which turns of it
    // count as the same photograph.
    //
    // `pixel_hash` was doing both jobs and could do neither: it hashes a grey
    // 128x128 square made for judging likeness, so colour, aspect and
    // resolution are gone from it long before the comparison. Calling that an
    // exact copy was a promise the evidence did not carry.
    //
    // `content_hash` is the frame as it is shown — colour, native size — and
    // it is what a copy has to match now. `phash_canon` is the perceptual
    // hash of whichever of the eight turns of the frame reads smallest, so a
    // photograph and its quarter-turned twin finally meet.
    //
    // Both are filled by reading the file, so an archive indexed before this
    // has them empty until it is read again; everything falls back to the old
    // behaviour meanwhile.
    r#"
    ALTER TABLE files ADD COLUMN content_hash BLOB;
    ALTER TABLE files ADD COLUMN phash_canon  INTEGER;
    CREATE INDEX files_content_hash ON files(content_hash);
    "#,
    // 014 — one decision per group, and the ability to know which was last.
    //
    // Choosing what to keep wrote a row and never took the previous one back,
    // so a group could hold several "kept" files at once. Rebuilding then
    // picked whichever came first, while the file the user had chosen earlier
    // was also sitting in `manual_rejects` from the later choice — and the
    // plan offered to move both of them. A group could empty itself.
    //
    // The timestamp makes "the last word wins" expressible. For decisions
    // already taken there is no way to tell which came last, so a group with
    // more than one is left without a manual choice at all: the tool goes
    // back to deciding for itself, which it can explain, rather than keeping
    // an answer nobody can account for. A file marked as kept and set aside
    // at once is a contradiction; the marking-aside is dropped.
    r#"
    ALTER TABLE manual_keepers ADD COLUMN marked_at INTEGER NOT NULL DEFAULT 0;

    DELETE FROM manual_keepers WHERE file_id IN (
        SELECT k.file_id FROM manual_keepers k
          JOIN family_members m ON m.file_id = k.file_id
         WHERE (SELECT COUNT(*) FROM manual_keepers k2
                  JOIN family_members m2 ON m2.file_id = k2.file_id
                 WHERE m2.family_id = m.family_id) > 1
    );

    DELETE FROM manual_rejects WHERE file_id IN (SELECT file_id FROM manual_keepers);
    "#,
    // 015 — what exactly moved, written down before anything moves.
    //
    // A journal row held the photograph alone. Its sidecars travelled after
    // it, unrecorded, and an undo went looking for them by name in the
    // destination directory — so a stranger's `photo.xmp` that happened to
    // sit in quarantine was carried into someone else's folder, and a
    // sidecar whose rename had quietly failed was never missed.
    //
    // `manifest` is the list of `src`/`dst` pairs of one operation: written
    // before the first rename, rewritten afterwards with the pairs that
    // actually moved. Undo and purge follow it instead of guessing. Rows
    // written before this migration have none, and keep the old behaviour —
    // it is all they have.
    r#"
    ALTER TABLE journal ADD COLUMN manifest TEXT;
    "#,
    // 016 — columns that were never filled, and one that was filled with a
    // word that stopped being true.
    //
    // `family_members.tier`, `families.key_value` and `files.full_hash` were
    // laid down for ideas that took a different shape: evidence became roles
    // and hashes, the group key became a kind alone, and identity is proved
    // by `content_hash` or by the bytes themselves at the moment of the move.
    // Nothing has ever written them.
    //
    // `journal.target_kind` was written as 'derived-bundle' for everything,
    // photographs and reorganisations included, so a row stated a kind it was
    // not. Nothing read it; what the row is about is in `op`. The index it
    // shared is rebuilt on the id alone.
    r#"
    DROP INDEX IF EXISTS journal_target;
    ALTER TABLE journal        DROP COLUMN target_kind;
    ALTER TABLE family_members DROP COLUMN tier;
    ALTER TABLE families       DROP COLUMN key_value;
    ALTER TABLE files          DROP COLUMN full_hash;
    CREATE INDEX journal_target ON journal(target_id);
    "#,
];

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL);")?;
    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    for (idx, sql) in MIGRATIONS.iter().enumerate() {
        let version = idx as i64 + 1;
        if version <= current {
            continue;
        }
        tracing::debug!(version, "применяю миграцию");
        conn.execute_batch("BEGIN")?;
        conn.execute_batch(sql)?;
        conn.execute("INSERT INTO schema_version(version) VALUES (?1)", [version])?;
        conn.execute_batch("COMMIT")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bring a connection up to `version` and no further, the way a database
    /// left by an older release looks.
    fn database_of_version(version: usize) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version(version INTEGER NOT NULL);")
            .unwrap();
        for (idx, sql) in MIGRATIONS.iter().take(version).enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.execute(
                "INSERT INTO schema_version(version) VALUES (?1)",
                [idx as i64 + 1],
            )
            .unwrap();
        }
        conn
    }

    /// The schema of a database, as SQLite itself describes it.
    fn shape(conn: &Connection) -> Vec<String> {
        let mut st = conn
            .prepare("SELECT type, name, COALESCE(sql, '') FROM sqlite_master ORDER BY type, name")
            .unwrap();
        let rows: Vec<String> = st
            .query_map([], |r| {
                Ok(format!(
                    "{} {} {}",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    // SQLite keeps the text of a CREATE as it was written, and
                    // ALTER rewrites it; only the words matter here.
                    r.get::<_, String>(2)?
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows
    }

    #[test]
    fn a_database_of_any_age_ends_up_shaped_like_a_new_one() {
        // Fourteen releases have gone out, so a database in use may have been
        // left at any version. Each of them has to arrive at exactly the
        // schema a fresh install has — otherwise a query written for today
        // meets a table from a year ago, which is the kind of failure that
        // only happens on someone else's archive.
        let fresh = Connection::open_in_memory().unwrap();
        migrate(&fresh).unwrap();
        let want = shape(&fresh);

        for version in 0..MIGRATIONS.len() {
            let conn = database_of_version(version);
            migrate(&conn).unwrap();
            assert_eq!(
                shape(&conn),
                want,
                "база версии {version} обновилась не в ту схему"
            );
            let broken: i64 = conn
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(broken, 0, "версия {version}: битые ссылки после обновления");
        }
    }

    #[test]
    fn dropping_columns_keeps_everything_else_in_place() {
        // Migration 016 takes away four columns nothing ever wrote to. A
        // database that has been in use since before it is exactly what must
        // survive that, so this one is filled the way a working archive fills
        // it and then brought up to date.
        let conn = database_of_version(15);
        conn.execute_batch(
            "INSERT INTO runs(id, started_at, roots, tool_version)
                  VALUES (1, 100, '[\"/archive\"]', 'old');
             INSERT INTO files(id, path, name, disk, dev, inode, nlink, size, mtime,
                               indexed_run, full_hash)
                  VALUES (1, '/archive/a.jpg', 'a.jpg', 'root', 1, 2, 1, 10, 5, 1, X'00ff');
             INSERT INTO families(id, key_kind, key_value, keeper_file, built_run)
                  VALUES (1, 'burst', 'whatever', 1, 1);
             INSERT INTO family_members(family_id, file_id, role, tier)
                  VALUES (1, 1, 'original', 'strong');
             INSERT INTO journal(id, run_id, target_kind, target_id, op, src, size,
                                 file_count, status, applied_at)
                  VALUES (1, 1, 'derived-bundle', 1, 'quarantine-file', '/archive/a.jpg',
                          10, 1, 'done', 100);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let path: String = conn
            .query_row("SELECT path FROM files WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(path, "/archive/a.jpg");
        let role: String = conn
            .query_row("SELECT role FROM family_members", [], |r| r.get(0))
            .unwrap();
        assert_eq!(role, "original");
        let (op, target): (String, i64) = conn
            .query_row("SELECT op, target_id FROM journal", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((op.as_str(), target), ("quarantine-file", 1));
        // The columns are gone, and the journal can still be looked up by the
        // id its index was rebuilt on.
        for sql in [
            "SELECT full_hash FROM files",
            "SELECT tier FROM family_members",
            "SELECT key_value FROM families",
            "SELECT target_kind FROM journal",
        ] {
            assert!(conn.prepare(sql).is_err(), "колонка осталась: {sql}");
        }
        assert!(conn
            .prepare("SELECT id FROM journal WHERE target_id = 1")
            .is_ok());
        let violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
    }
}
