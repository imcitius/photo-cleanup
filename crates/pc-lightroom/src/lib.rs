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
            bail!(
                "{}",
                pc_core::tf!(
                    "каталог не найден: {0}",
                    "catalogue not found: {0}",
                    catalog.display()
                )
            );
        }
        let tmp = tempfile::tempdir().context(pc_core::tr!(
            "не удалось создать временный каталог",
            "could not create a temporary directory"
        ))?;
        let name = catalog
            .file_name()
            .context(pc_core::tr!(
                "путь без имени файла",
                "the path has no file name"
            ))?
            .to_owned();
        let dst = tmp.path().join(&name);
        std::fs::copy(catalog, &dst).with_context(|| {
            pc_core::tf!(
                "не удалось скопировать {0}",
                "could not copy {0}",
                catalog.display()
            )
        })?;

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

        let conn = Connection::open(&dst).with_context(|| {
            pc_core::tf!(
                "не удалось открыть копию каталога {0}",
                "could not open the catalogue copy {0}",
                dst.display()
            )
        })?;
        Ok(Self { conn, _tmp: tmp })
    }

    /// Number of master files referenced by the catalog. Used both for the
    /// rebuild-cost hint and as the denominator of the smart-preview gate.
    pub fn file_count(&self) -> Result<i64> {
        if !table_exists(&self.conn, "AgLibraryFile") {
            bail!(
                "{}",
                pc_core::tr!(
                    "нет таблицы AgLibraryFile (незнакомая версия схемы)",
                    "no AgLibraryFile table (unfamiliar schema version)"
                )
            );
        }
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM AgLibraryFile", [], |r| r.get(0))?)
    }

    /// Every master the catalog points at, with the judgements the
    /// photographer already made about it.
    ///
    /// A five-star frame with develop history is the last thing that should
    /// ever be proposed for deletion, and the catalog is the only place that
    /// knows it.
    pub fn entries(&self) -> Result<Vec<CatalogEntry>> {
        for t in ["AgLibraryFile", "AgLibraryFolder", "AgLibraryRootFolder"] {
            if !table_exists(&self.conn, t) {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "нет таблицы {0} (незнакомая версия схемы)",
                        "no {0} table (unfamiliar schema version)",
                        t
                    )
                );
            }
        }
        // Ratings live in a table that has moved between versions, so the
        // join is optional: losing a rating is a worse outcome than losing
        // the protection itself.
        let has_images = table_exists(&self.conn, "Adobe_images");
        let sql = if has_images {
            "SELECT rf.absolutePath, fo.pathFromRoot, f.idx_filename,
                    i.rating, i.pick, i.fileFormat
               FROM AgLibraryFile f
               JOIN AgLibraryFolder fo     ON f.folder = fo.id_local
               JOIN AgLibraryRootFolder rf ON fo.rootFolder = rf.id_local
               LEFT JOIN Adobe_images i    ON i.rootFile = f.id_local"
        } else {
            "SELECT rf.absolutePath, fo.pathFromRoot, f.idx_filename,
                    NULL, NULL, NULL
               FROM AgLibraryFile f
               JOIN AgLibraryFolder fo     ON f.folder = fo.id_local
               JOIN AgLibraryRootFolder rf ON fo.rootFolder = rf.id_local"
        };
        let mut st = self.conn.prepare(sql)?;
        let rows = st.query_map([], |r| {
            let root: String = r.get(0)?;
            let rel: Option<String> = r.get(1)?;
            let file: String = r.get(2)?;
            Ok(CatalogEntry {
                path: join_catalog_path(&root, rel.as_deref().unwrap_or(""), &file),
                rating: r.get::<_, Option<f64>>(3)?.map(|v| v as i64),
                pick: r.get::<_, Option<f64>>(4)?.map(|v| v as i64),
                format: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Absolute paths of every master file the catalog points at.
    pub fn original_paths(&self) -> Result<Vec<String>> {
        for t in ["AgLibraryFile", "AgLibraryFolder", "AgLibraryRootFolder"] {
            if !table_exists(&self.conn, t) {
                bail!(
                    "{}",
                    pc_core::tf!(
                        "нет таблицы {0} (незнакомая версия схемы)",
                        "no {0} table (unfamiliar schema version)",
                        t
                    )
                );
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

/// One master file as the catalog sees it.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub path: String,
    /// Stars, 0 to 5.
    pub rating: Option<i64>,
    /// Flag: 1 picked, -1 rejected.
    pub pick: Option<i64>,
    pub format: Option<String>,
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
    fn reads_ratings_when_the_catalog_has_them() {
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("Rated.lrcat");
        let conn = Connection::open(&cat).unwrap();
        conn.execute_batch(
            "CREATE TABLE AgLibraryRootFolder(id_local INTEGER PRIMARY KEY, absolutePath TEXT);
             CREATE TABLE AgLibraryFolder(id_local INTEGER PRIMARY KEY, pathFromRoot TEXT, rootFolder INTEGER);
             CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, folder INTEGER, idx_filename TEXT);
             CREATE TABLE Adobe_images(id_local INTEGER PRIMARY KEY, rootFile INTEGER, rating REAL, pick REAL, fileFormat TEXT);
             INSERT INTO AgLibraryRootFolder VALUES (1, '/photos/');
             INSERT INTO AgLibraryFolder     VALUES (1, '', 1);
             INSERT INTO AgLibraryFile       VALUES (1, 1, 'keep.arw'), (2, 1, 'meh.arw');
             INSERT INTO Adobe_images        VALUES (1, 1, 5.0, 1.0, 'RAW'), (2, 2, NULL, NULL, 'RAW');",
        )
        .unwrap();
        drop(conn);

        let e = CatalogReader::open(&cat).unwrap().entries().unwrap();
        assert_eq!(e.len(), 2);
        let five = e.iter().find(|x| x.path.ends_with("keep.arw")).unwrap();
        assert_eq!(five.rating, Some(5));
        assert_eq!(five.pick, Some(1));
        let plain = e.iter().find(|x| x.path.ends_with("meh.arw")).unwrap();
        assert_eq!(plain.rating, None);
    }

    #[test]
    fn a_catalog_without_the_images_table_still_yields_its_masters() {
        // Protection matters more than the rating that decorates it.
        let tmp = tempfile::tempdir().unwrap();
        let cat = tmp.path().join("Old.lrcat");
        let conn = Connection::open(&cat).unwrap();
        conn.execute_batch(
            "CREATE TABLE AgLibraryRootFolder(id_local INTEGER PRIMARY KEY, absolutePath TEXT);
             CREATE TABLE AgLibraryFolder(id_local INTEGER PRIMARY KEY, pathFromRoot TEXT, rootFolder INTEGER);
             CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, folder INTEGER, idx_filename TEXT);
             INSERT INTO AgLibraryRootFolder VALUES (1, '/photos/');
             INSERT INTO AgLibraryFolder     VALUES (1, '2019/', 1);
             INSERT INTO AgLibraryFile       VALUES (1, 1, 'a.arw');",
        )
        .unwrap();
        drop(conn);
        let e = CatalogReader::open(&cat).unwrap().entries().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].path, "/photos/2019/a.arw");
        assert_eq!(e[0].rating, None);
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
