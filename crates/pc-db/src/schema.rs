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
