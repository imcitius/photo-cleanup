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
