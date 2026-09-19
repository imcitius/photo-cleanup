//! The image index.

use anyhow::Result;
use rusqlite::{params, OptionalExtension, Row};

use crate::Db;

#[derive(Debug, Clone, Default)]
pub struct NewFile {
    pub path: String,
    pub name: String,
    pub disk: String,
    pub dev: i64,
    pub inode: i64,
    pub nlink: i64,
    pub size: i64,
    pub mtime: i64,
    pub container: Option<String>,
    pub extension_lied: bool,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub pixel_source: Option<String>,
    pub partial_hash: Option<Vec<u8>>,
    pub pixel_hash: Option<Vec<u8>>,
    pub phash: Option<i64>,
    pub dhash: Option<i64>,
    pub phash_crops: Option<Vec<u8>>,
    pub thumb_key: Option<String>,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewMeta {
    pub taken_at: Option<i64>,
    pub date_source: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub body_serial: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub f_number: Option<f64>,
    pub focal_length: Option<f64>,
    pub exposure: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    pub software: Option<String>,
    pub xmp_document_id: Option<String>,
    pub xmp_original_id: Option<String>,
    pub xmp_derived_from: Option<String>,
    pub dng_original_raw: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FileRow {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub size: i64,
    pub container: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub pixel_source: Option<String>,
    pub partial_hash: Option<Vec<u8>>,
    pub pixel_hash: Option<Vec<u8>>,
    pub phash: Option<i64>,
    pub dhash: Option<i64>,
    pub thumb_key: Option<String>,
    pub skipped_reason: Option<String>,
}

impl FileRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            path: r.get("path")?,
            name: r.get("name")?,
            size: r.get("size")?,
            container: r.get("container")?,
            width: r.get("width")?,
            height: r.get("height")?,
            pixel_source: r.get("pixel_source")?,
            partial_hash: r.get("partial_hash")?,
            pixel_hash: r.get("pixel_hash")?,
            phash: r.get("phash")?,
            dhash: r.get("dhash")?,
            thumb_key: r.get("thumb_key")?,
            skipped_reason: r.get("skipped_reason")?,
        })
    }

    pub fn pixels(&self) -> i64 {
        self.width.unwrap_or(0) * self.height.unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct IndexStats {
    pub total: i64,
    pub images: i64,
    pub skipped: i64,
}

impl Db {
    /// Whether this exact file has already been indexed.
    ///
    /// Identity is `(path, size, mtime, inode)` — the same key the whole tool
    /// uses, so an interrupted run resumes instead of starting over.
    pub fn file_is_current(&self, path: &str, size: i64, mtime: i64, inode: i64) -> Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM files
                  WHERE path = ?1 AND size = ?2 AND mtime = ?3 AND inode = ?4
                    AND (phash IS NOT NULL OR skipped_reason IS NOT NULL)",
                params![path, size, mtime, inode],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    pub fn upsert_file(&self, f: &NewFile, run_id: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO files(path, name, disk, dev, inode, nlink, size, mtime, container,
                               extension_lied, width, height, orientation, pixel_source,
                               partial_hash, pixel_hash, phash, dhash, phash_crops, thumb_key,
                               skipped_reason, indexed_run, first_seen_run, last_seen_run)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                     ?21,?22,?22,?22)
             ON CONFLICT(path) DO UPDATE SET
                 name=excluded.name, disk=excluded.disk, dev=excluded.dev,
                 inode=excluded.inode, nlink=excluded.nlink, size=excluded.size,
                 mtime=excluded.mtime, container=excluded.container,
                 extension_lied=excluded.extension_lied, width=excluded.width,
                 height=excluded.height, orientation=excluded.orientation,
                 pixel_source=excluded.pixel_source, partial_hash=excluded.partial_hash,
                 pixel_hash=excluded.pixel_hash, phash=excluded.phash, dhash=excluded.dhash,
                 phash_crops=excluded.phash_crops, thumb_key=excluded.thumb_key,
                 skipped_reason=excluded.skipped_reason, indexed_run=excluded.indexed_run,
                 last_seen_run=excluded.last_seen_run",
            params![
                f.path,
                f.name,
                f.disk,
                f.dev,
                f.inode,
                f.nlink,
                f.size,
                f.mtime,
                f.container,
                f.extension_lied as i64,
                f.width,
                f.height,
                f.orientation,
                f.pixel_source,
                f.partial_hash,
                f.pixel_hash,
                f.phash,
                f.dhash,
                f.phash_crops,
                f.thumb_key,
                f.skipped_reason,
                run_id
            ],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![f.path],
            |r| r.get(0),
        )?)
    }

    pub fn upsert_meta(&self, file_id: i64, m: &NewMeta) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta(file_id, taken_at, date_source, camera_make, camera_model,
                              body_serial, lens, iso, f_number, focal_length, exposure,
                              gps_lat, gps_lon, software, xmp_document_id, xmp_original_id,
                              xmp_derived_from, dng_original_raw)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)
             ON CONFLICT(file_id) DO UPDATE SET
                 taken_at=excluded.taken_at, date_source=excluded.date_source,
                 camera_make=excluded.camera_make, camera_model=excluded.camera_model,
                 body_serial=excluded.body_serial, lens=excluded.lens, iso=excluded.iso,
                 f_number=excluded.f_number, focal_length=excluded.focal_length,
                 exposure=excluded.exposure, gps_lat=excluded.gps_lat, gps_lon=excluded.gps_lon,
                 software=excluded.software, xmp_document_id=excluded.xmp_document_id,
                 xmp_original_id=excluded.xmp_original_id,
                 xmp_derived_from=excluded.xmp_derived_from,
                 dng_original_raw=excluded.dng_original_raw",
            params![
                file_id,
                m.taken_at,
                m.date_source,
                m.camera_make,
                m.camera_model,
                m.body_serial,
                m.lens,
                m.iso,
                m.f_number,
                m.focal_length,
                m.exposure,
                m.gps_lat,
                m.gps_lon,
                m.software,
                m.xmp_document_id,
                m.xmp_original_id,
                m.xmp_derived_from,
                m.dng_original_raw
            ],
        )?;
        Ok(())
    }

    pub fn index_stats(&self) -> Result<IndexStats> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(phash IS NOT NULL), 0),
                    COALESCE(SUM(skipped_reason IS NOT NULL), 0)
               FROM files",
            [],
            |r| {
                Ok(IndexStats {
                    total: r.get(0)?,
                    images: r.get(1)?,
                    skipped: r.get(2)?,
                })
            },
        )?)
    }

    /// Every indexed image, for the matching stage.
    pub fn all_images(&self) -> Result<Vec<FileRow>> {
        let mut st = self
            .conn
            .prepare("SELECT * FROM files WHERE phash IS NOT NULL ORDER BY id")?;
        let rows = st
            .query_map([], FileRow::from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn file(&self, id: i64) -> Result<Option<FileRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM files WHERE id = ?1",
                params![id],
                FileRow::from_row,
            )
            .optional()?)
    }

    /// Container breakdown, for the scan report.
    pub fn container_counts(&self) -> Result<Vec<(String, i64, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT COALESCE(container, 'не изображение'), COUNT(*), COALESCE(SUM(size),0)
               FROM files GROUP BY 1 ORDER BY 3 DESC",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Files whose extension disagreed with their magic bytes.
    pub fn mislabelled_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM files WHERE extension_lied = 1",
            [],
            |r| r.get(0),
        )?)
    }
}
