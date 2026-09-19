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

/// Everything the grouping stage needs about one file, in a single row.
#[derive(Debug, Clone, Default)]
pub struct FileInfo {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub size: i64,
    pub container: String,
    pub width: i64,
    pub height: i64,
    pub pixel_source: String,
    pub partial_hash: Option<Vec<u8>>,
    pub pixel_hash: Option<Vec<u8>>,
    pub phash: u64,
    pub dhash: u64,
    pub crops: [u64; 5],
    pub thumb_key: Option<String>,
    pub taken_at: Option<i64>,
    pub camera_model: Option<String>,
    pub body_serial: Option<String>,
    pub software: Option<String>,
    pub doc_id: Option<String>,
    pub orig_doc_id: Option<String>,
    pub derived_from: Option<String>,
    pub dng_original_raw: Option<String>,
}

impl FileInfo {
    pub fn pixels(&self) -> i64 {
        self.width * self.height
    }

    /// Filename without its extension, which is how a camera pairs a raw file
    /// with the JPEG it wrote beside it.
    pub fn stem(&self) -> &str {
        self.name
            .rsplit_once('.')
            .map_or(self.name.as_str(), |(a, _)| a)
    }

    pub fn dir(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(a, _)| a)
    }

    pub fn is_raw(&self) -> bool {
        self.container == "tiff" && self.pixel_source == "preview"
    }
}

fn crops_from(blob: Option<Vec<u8>>) -> [u64; 5] {
    let mut out = [0u64; 5];
    if let Some(b) = blob {
        for (i, chunk) in b.as_chunks::<8>().0.iter().take(5).enumerate() {
            out[i] = u64::from_le_bytes(*chunk);
        }
    }
    out
}

impl Db {
    /// Every successfully indexed image, joined with its metadata.
    pub fn all_indexed(&self) -> Result<Vec<FileInfo>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path, f.name, f.size, f.container, f.width, f.height,
                    f.pixel_source, f.partial_hash, f.pixel_hash, f.phash, f.dhash,
                    f.phash_crops, f.thumb_key,
                    m.taken_at, m.camera_model, m.body_serial, m.software,
                    m.xmp_document_id, m.xmp_original_id, m.xmp_derived_from,
                    m.dng_original_raw
               FROM files f LEFT JOIN meta m ON m.file_id = f.id
              WHERE f.phash IS NOT NULL
              ORDER BY f.id",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(FileInfo {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    size: r.get(3)?,
                    container: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    width: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    height: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    pixel_source: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    partial_hash: r.get(8)?,
                    pixel_hash: r.get(9)?,
                    phash: r.get::<_, i64>(10)? as u64,
                    dhash: r.get::<_, i64>(11)? as u64,
                    crops: crops_from(r.get(12)?),
                    thumb_key: r.get(13)?,
                    taken_at: r.get(14)?,
                    camera_model: r.get(15)?,
                    body_serial: r.get(16)?,
                    software: r.get(17)?,
                    doc_id: r.get(18)?,
                    orig_doc_id: r.get(19)?,
                    derived_from: r.get(20)?,
                    dng_original_raw: r.get(21)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

impl Db {
    pub fn clear_families(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM family_members; DELETE FROM families;")?;
        Ok(())
    }

    pub fn insert_family(
        &self,
        key_kind: &str,
        taken_at: Option<i64>,
        camera: Option<&str>,
        keeper: Option<i64>,
        run_id: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO families(key_kind, taken_at, camera, keeper_file, built_run)
             VALUES (?1,?2,?3,?4,?5)",
            params![key_kind, taken_at, camera, keeper, run_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_family_member(
        &self,
        family_id: i64,
        file_id: i64,
        role: &str,
        evidence: Option<&str>,
        quality: f64,
        breakdown: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO family_members(family_id, file_id, role, evidence,
                                                   quality, breakdown)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![family_id, file_id, role, evidence, quality, breakdown],
        )?;
        Ok(())
    }

    pub fn role_counts(&self) -> Result<Vec<(String, i64, i64)>> {
        let mut st = self.conn.prepare(
            "SELECT fm.role, COUNT(*), COALESCE(SUM(f.size), 0)
               FROM family_members fm JOIN files f ON f.id = fm.file_id
              GROUP BY fm.role ORDER BY 3 DESC",
        )?;
        let rows = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[derive(Debug, Clone)]
pub struct MemberRow {
    pub file_id: i64,
    pub path: String,
    pub name: String,
    pub role: String,
    pub size: i64,
    pub width: i64,
    pub height: i64,
    pub container: String,
    pub quality: f64,
    pub breakdown: String,
    pub evidence: Option<String>,
    pub thumb_key: Option<String>,
    pub is_keeper: bool,
}

#[derive(Debug, Clone)]
pub struct FamilyRow {
    pub id: i64,
    pub key_kind: String,
    pub taken_at: Option<i64>,
    pub camera: Option<String>,
    pub members: Vec<MemberRow>,
}

impl FamilyRow {
    pub fn total_size(&self) -> i64 {
        self.members.iter().map(|m| m.size).sum()
    }
}

impl Db {
    /// Families with more than one member, largest reclaimable first.
    pub fn families(&self, only_multi: bool, limit: i64, offset: i64) -> Result<Vec<FamilyRow>> {
        let having = if only_multi {
            "HAVING COUNT(*) > 1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT fa.id FROM families fa
               JOIN family_members fm ON fm.family_id = fa.id
               JOIN files f ON f.id = fm.file_id
              GROUP BY fa.id {having}
              ORDER BY SUM(CASE WHEN fm.role = 'copy' THEN f.size ELSE 0 END) DESC,
                       SUM(f.size) DESC
              LIMIT ?1 OFFSET ?2"
        );
        let mut st = self.conn.prepare(&sql)?;
        let ids = st
            .query_map(params![limit, offset], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);

        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(f) = self.family(id)? {
                out.push(f);
            }
        }
        Ok(out)
    }

    pub fn family(&self, id: i64) -> Result<Option<FamilyRow>> {
        let head = self
            .conn
            .query_row(
                "SELECT id, key_kind, taken_at, camera, keeper_file FROM families WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, key_kind, taken_at, camera, keeper)) = head else {
            return Ok(None);
        };

        let mut st = self.conn.prepare(
            "SELECT fm.file_id, f.path, f.name, fm.role, f.size, f.width, f.height,
                    f.container, fm.quality, fm.breakdown, fm.evidence, f.thumb_key
               FROM family_members fm JOIN files f ON f.id = fm.file_id
              WHERE fm.family_id = ?1
              ORDER BY fm.quality DESC",
        )?;
        let members = st
            .query_map(params![id], |r| {
                let file_id: i64 = r.get(0)?;
                Ok(MemberRow {
                    file_id,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    role: r.get(3)?,
                    size: r.get(4)?,
                    width: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    height: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    container: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    quality: r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                    breakdown: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    evidence: r.get(10)?,
                    thumb_key: r.get(11)?,
                    is_keeper: Some(file_id) == keeper,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(Some(FamilyRow {
            id,
            key_kind,
            taken_at,
            camera,
            members,
        }))
    }

    pub fn family_count(&self, only_multi: bool) -> Result<i64> {
        let sql = if only_multi {
            "SELECT COUNT(*) FROM (SELECT family_id FROM family_members
                                    GROUP BY family_id HAVING COUNT(*) > 1)"
        } else {
            "SELECT COUNT(*) FROM families"
        };
        Ok(self.conn.query_row(sql, [], |r| r.get(0))?)
    }
}

impl Db {
    /// Point a family at a different member as its best version.
    /// Returns false when the file is not part of that family.
    pub fn set_family_keeper(&self, family_id: i64, file_id: i64) -> Result<bool> {
        let belongs: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM family_members WHERE family_id = ?1 AND file_id = ?2",
                params![family_id, file_id],
                |r| r.get(0),
            )
            .optional()?;
        if belongs.is_none() {
            return Ok(false);
        }
        self.conn.execute(
            "UPDATE families SET keeper_file = ?1 WHERE id = ?2",
            params![file_id, family_id],
        )?;
        Ok(true)
    }
}
