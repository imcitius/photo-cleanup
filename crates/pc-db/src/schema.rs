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
