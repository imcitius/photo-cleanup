//! Read-only access to Lightroom Classic catalogs.
//!
//! The catalog is a SQLite database, but it is the user's live working file:
//! we never open the original read-write, and we never rely on being able to
//! create the `-wal`/`-shm` files next to it. Instead the catalog and its
//! sidecars are copied into a temporary directory and the copy is opened.
//!
//! The schema differs between Lightroom versions, so every query is guarded:
//! a catalog we cannot understand degrades to "unreadable", which blocks the
//! smart-preview gate rather than silently passing it.

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct CatalogReader {
    conn: Connection,
    // Kept alive so the copies survive as long as the connection.
    _tmp: tempfile::TempDir,
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .is_ok()
}

impl CatalogReader {
    pub fn open(catalog: &Path) -> Result<Self> {
        if !catalog.is_file() {
            bail!("каталог не найден: {}", catalog.display());
        }
        let tmp = tempfile::tempdir().context("не удалось создать временный каталог")?;
        let name = catalog
            .file_name()
            .context("путь без имени файла")?
            .to_owned();
        let dst = tmp.path().join(&name);
        std::fs::copy(catalog, &dst)
            .with_context(|| format!("не удалось скопировать {}", catalog.display()))?;

        // Copy the sidecars too when present, so the copy is self-consistent.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut side = catalog.as_os_str().to_owned();
            side.push(suffix);
            let side = PathBuf::from(side);
            if side.is_file() {
                let mut dst_side = dst.as_os_str().to_owned();
                dst_side.push(suffix);
                let _ = std::fs::copy(&side, PathBuf::from(dst_side));
            }
        }

        let conn = Connection::open(&dst)
            .with_context(|| format!("не удалось открыть копию каталога {}", dst.display()))?;
        Ok(Self { conn, _tmp: tmp })
    }

    /// Number of master files referenced by the catalog. Used both for the
    /// rebuild-cost hint and as the denominator of the smart-preview gate.
    pub fn file_count(&self) -> Result<i64> {
        if !table_exists(&self.conn, "AgLibraryFile") {
            bail!("нет таблицы AgLibraryFile (незнакомая версия схемы)");
        }
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM AgLibraryFile", [], |r| r.get(0))?)
    }

    /// Absolute paths of every master file the catalog points at.
    pub fn original_paths(&self) -> Result<Vec<String>> {
        for t in ["AgLibraryFile", "AgLibraryFolder", "AgLibraryRootFolder"] {
            if !table_exists(&self.conn, t) {
                bail!("нет таблицы {t} (незнакомая версия схемы)");
            }
        }
        let mut st = self.conn.prepare(
            "SELECT rf.absolutePath, fo.pathFromRoot, f.idx_filename
               FROM AgLibraryFile f
               JOIN AgLibraryFolder fo     ON f.folder = fo.id_local
               JOIN AgLibraryRootFolder rf ON fo.rootFolder = rf.id_local",
        )?;
        let rows = st.query_map([], |r| {
            let root: String = r.get(0)?;
            let rel: Option<String> = r.get(1)?;
            let file: String = r.get(2)?;
            Ok(join_catalog_path(
                &root,
                rel.as_deref().unwrap_or(""),
                &file,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Lightroom stores `absolutePath` and `pathFromRoot` with trailing slashes,
/// but not consistently across versions.
fn join_catalog_path(root: &str, rel: &str, file: &str) -> String {
    let mut s = String::with_capacity(root.len() + rel.len() + file.len() + 2);
    s.push_str(root.trim_end_matches('/'));
    let rel = rel.trim_matches('/');
    if !rel.is_empty() {
        s.push('/');
        s.push_str(rel);
    }
    s.push('/');
    s.push_str(file);
    s
}

/// Result of checking whether a catalog's masters are all reachable.
#[derive(Debug, Clone, Copy)]
pub struct OriginalsCheck {
    pub total: u64,
    pub missing: u64,
}

impl OriginalsCheck {
    pub fn all_present(&self) -> bool {
        self.missing == 0
    }
}

/// Stat every master file. This is the gate for deleting smart previews:
/// they are only regenerable while the originals they proxy are reachable.
pub fn check_originals(catalog: &Path) -> Result<OriginalsCheck> {
    let reader = CatalogReader::open(catalog)?;
    let paths = reader.original_paths()?;
    let total = paths.len() as u64;
    let missing = paths.iter().filter(|p| !Path::new(p).exists()).count() as u64;
    Ok(OriginalsCheck { total, missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_paths_with_and_without_slashes() {
        assert_eq!(
            join_catalog_path("/mnt/disk3/foto/", "2019/07/", "DSC01234.ARW"),
            "/mnt/disk3/foto/2019/07/DSC01234.ARW"
        );
        assert_eq!(
            join_catalog_path("/mnt/disk3/foto", "", "DSC01234.ARW"),
            "/mnt/disk3/foto/DSC01234.ARW"
        );
    }

    #[test]
    fn reads_a_minimal_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("Test.lrcat");
        let conn = Connection::open(&cat).unwrap();
        conn.execute_batch(
            "CREATE TABLE AgLibraryRootFolder(id_local INTEGER PRIMARY KEY, absolutePath TEXT);
             CREATE TABLE AgLibraryFolder(id_local INTEGER PRIMARY KEY, pathFromRoot TEXT, rootFolder INTEGER);
             CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, folder INTEGER, idx_filename TEXT);
             INSERT INTO AgLibraryRootFolder VALUES (1, '/photos/');
             INSERT INTO AgLibraryFolder     VALUES (1, '2019/', 1);
             INSERT INTO AgLibraryFile       VALUES (1, 1, 'a.arw'), (2, 1, 'b.arw');",
        )
        .unwrap();
        drop(conn);

        let r = CatalogReader::open(&cat).unwrap();
        assert_eq!(r.file_count().unwrap(), 2);
        assert_eq!(
            r.original_paths().unwrap(),
            vec!["/photos/2019/a.arw", "/photos/2019/b.arw"]
        );
    }

    #[test]
    fn unknown_schema_is_an_error_not_a_pass() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("Weird.lrcat");
        Connection::open(&cat)
            .unwrap()
            .execute_batch("CREATE TABLE something_else(x INTEGER);")
            .unwrap();
        let r = CatalogReader::open(&cat).unwrap();
        assert!(r.file_count().is_err());
        assert!(r.original_paths().is_err());
    }
}
