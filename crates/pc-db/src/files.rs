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
    pub content_hash: Option<Vec<u8>>,
    pub phash_canon: Option<i64>,
    pub phash: Option<i64>,
    pub dhash: Option<i64>,
    pub phash_crops: Option<Vec<u8>>,
    pub thumb_key: Option<String>,
    pub skipped_reason: Option<String>,
    pub sharpness: Option<f64>,
    pub clip_low: Option<f64>,
    pub clip_high: Option<f64>,
    pub entropy: Option<f64>,
    pub contrast: Option<f64>,
    pub saturation: Option<f64>,
    pub chroma: Option<f64>,
    pub tonal_range: Option<f64>,
    pub white_fraction: Option<f64>,
    pub bimodality: Option<f64>,
    pub text_rows: Option<f64>,
    pub text_banding: Option<f64>,
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
    /// `present` while the file is in the archive, `quarantined` once moved.
    pub state: String,
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
            state: r.get("state")?,
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
    /// Indexed frames with no thumbnail to show for them. Non-zero means a
    /// grey square somewhere in the interface, and an index run repairs it.
    pub without_thumb: i64,
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
                // A file counts as done only if it has a thumbnail to show
                // and the evidence a copy is judged by — or a reason why it
                // never will. Without that a frame whose thumbnail failed
                // stays a grey square for good, and an archive indexed before
                // the evidence existed never gains it: every later run sees
                // an indexed file and skips it.
                "SELECT 1 FROM files
                  WHERE path = ?1 AND size = ?2 AND mtime = ?3 AND inode = ?4
                    AND ((phash IS NOT NULL AND thumb_key IS NOT NULL
                          AND content_hash IS NOT NULL)
                         OR skipped_reason IS NOT NULL)",
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
                               skipped_reason, indexed_run, first_seen_run, last_seen_run,
                               sharpness, clip_low, clip_high, entropy, contrast,
                               saturation, white_fraction, bimodality, text_rows, text_banding,
                               chroma, tonal_range, content_hash, phash_canon)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                     ?21,?22,?22,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35,?36)
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
                 last_seen_run=excluded.last_seen_run, sharpness=excluded.sharpness,
                 clip_low=excluded.clip_low, clip_high=excluded.clip_high,
                 entropy=excluded.entropy, contrast=excluded.contrast,
                 saturation=excluded.saturation, chroma=excluded.chroma,
                 tonal_range=excluded.tonal_range, content_hash=excluded.content_hash,
                 phash_canon=excluded.phash_canon, white_fraction=excluded.white_fraction,
                 bimodality=excluded.bimodality, text_rows=excluded.text_rows,
                 text_banding=excluded.text_banding",
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
                run_id,
                f.sharpness,
                f.clip_low,
                f.clip_high,
                f.entropy,
                f.contrast,
                f.saturation,
                f.white_fraction,
                f.bimodality,
                f.text_rows,
                f.text_banding,
                f.chroma,
                f.tonal_range,
                f.content_hash,
                f.phash_canon
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
                 taken_at=CASE WHEN meta.date_source='manual' THEN meta.taken_at ELSE excluded.taken_at END,
                 date_source=CASE WHEN meta.date_source='manual' THEN meta.date_source ELSE excluded.date_source END,
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
                    COALESCE(SUM(skipped_reason IS NOT NULL), 0),
                    COALESCE(SUM(phash IS NOT NULL AND thumb_key IS NULL), 0)
               FROM files",
            [],
            |r| {
                Ok(IndexStats {
                    total: r.get(0)?,
                    images: r.get(1)?,
                    skipped: r.get(2)?,
                    without_thumb: r.get(3)?,
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

    /// Where a file's bytes are right now.
    ///
    /// `files.path` records where a photograph belongs in the archive, and
    /// quarantining it deliberately does not rewrite that: the row has to keep
    /// saying where the file would go back to. So for anything that has been
    /// moved out, the live location is the journal's destination — without
    /// which the viewer cannot show a frame the user is about to delete
    /// forever, which is exactly when looking at it matters most.
    pub fn file_path_now(&self, id: i64) -> Result<Option<String>> {
        let Some(f) = self.file(id)? else {
            return Ok(None);
        };
        if f.state != "quarantined" {
            return Ok(Some(f.path));
        }
        let moved: Option<String> = self
            .conn
            .query_row(
                "SELECT dst FROM journal
                  WHERE target_id = ?1 AND status = 'done' AND dst IS NOT NULL
                  ORDER BY id DESC LIMIT 1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(moved.or(Some(f.path)))
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
    /// The frame as shown, hashed whole and in colour: the evidence a copy
    /// needs. Empty for archives indexed before it existed.
    pub content_hash: Option<Vec<u8>>,
    pub phash: u64,
    /// Perceptual hash of the frame's smallest-reading turn, so a rotated
    /// duplicate meets its original.
    pub phash_canon: u64,
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
    pub sharpness: Option<f64>,
    pub clip_low: Option<f64>,
    pub clip_high: Option<f64>,
    pub entropy: Option<f64>,
    pub contrast: Option<f64>,
    pub saturation: Option<f64>,
    pub chroma: Option<f64>,
    pub tonal_range: Option<f64>,
    pub white_fraction: Option<f64>,
    pub bimodality: Option<f64>,
    pub text_rows: Option<f64>,
    pub text_banding: Option<f64>,
    pub lens: Option<String>,
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
        pc_core::dir_name(&self.path)
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
                    m.dng_original_raw, m.lens,
                    f.sharpness, f.clip_low, f.clip_high, f.entropy, f.contrast,
                    f.saturation, f.white_fraction, f.bimodality, f.text_rows,
                    f.text_banding, f.chroma, f.tonal_range,
                    f.content_hash, f.phash_canon
               FROM files f LEFT JOIN meta m ON m.file_id = f.id
              WHERE f.phash IS NOT NULL AND f.state = 'present'
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
                    lens: r.get(22)?,
                    sharpness: r.get(23)?,
                    clip_low: r.get(24)?,
                    clip_high: r.get(25)?,
                    entropy: r.get(26)?,
                    contrast: r.get(27)?,
                    saturation: r.get(28)?,
                    white_fraction: r.get(29)?,
                    bimodality: r.get(30)?,
                    text_rows: r.get(31)?,
                    text_banding: r.get(32)?,
                    chroma: r.get(33)?,
                    tonal_range: r.get(34)?,
                    content_hash: r.get(35)?,
                    phash_canon: r.get::<_, Option<i64>>(36)?.unwrap_or(0) as u64,
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
    /// What the pixels hash to. A role says what a file is; this says whether
    /// it is the same picture as the one being kept.
    pub pixel_hash: Option<Vec<u8>>,
    /// The whole frame in colour: what "the same picture" means now.
    pub content_hash: Option<Vec<u8>>,
    /// The user looked at this file and said it can go, whatever its role.
    pub is_rejected: bool,
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
                    f.container, fm.quality, fm.breakdown, fm.evidence, f.thumb_key,
                    f.pixel_hash, r.file_id IS NOT NULL, f.content_hash
               FROM family_members fm
               JOIN files f ON f.id = fm.file_id
               LEFT JOIN manual_rejects r ON r.file_id = fm.file_id
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
                    pixel_hash: r.get(12)?,
                    is_rejected: r.get(13)?,
                    content_hash: r.get(14)?,
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
    /// Mark a file as moved out of the archive, or back into it.
    pub fn set_file_state(&self, file_id: i64, state: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET state = ?1 WHERE id = ?2",
            params![state, file_id],
        )?;
        Ok(())
    }

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

impl Db {
    pub fn replace_catalog_files(
        &self,
        catalog_id: i64,
        entries: &[(String, Option<i64>, Option<i64>)],
    ) -> Result<()> {
        self.conn.execute(
            "DELETE FROM lr_files WHERE catalog_id = ?1",
            params![catalog_id],
        )?;
        let mut st = self.conn.prepare(
            "INSERT OR REPLACE INTO lr_files(catalog_id, path, rating, pick)
             VALUES (?1,?2,?3,?4)",
        )?;
        for (path, rating, pick) in entries {
            st.execute(params![catalog_id, path, rating, pick])?;
        }
        Ok(())
    }

    /// Paths a live catalog points at, with the best rating any of them gave.
    pub fn lightroom_protected(&self) -> Result<std::collections::HashMap<String, Option<i64>>> {
        let mut st = self.conn.prepare(
            "SELECT l.path, MAX(l.rating)
               FROM lr_files l JOIN lr_catalogs c ON c.id = l.catalog_id
              WHERE c.is_backup = 0
              GROUP BY l.path",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Members of every family, with what the plan needs to judge them.
    /// Files the user marked as not worth keeping, with everything the
    /// planner needs to treat them like any other candidate.
    pub fn rejected_rows(&self) -> Result<Vec<PlanRow>> {
        self.rejected_rows_scoped(None)
    }

    /// Files the user set aside by hand, optionally only within one group.
    ///
    /// A hand-made decision carries no family of its own — it is about the
    /// file, not about what it duplicates — so a plan narrowed to one group
    /// has to ask for its rejects by that group.
    pub fn rejected_rows_scoped(&self, family: Option<i64>) -> Result<Vec<PlanRow>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path, f.size, f.width, f.height, f.mtime, f.inode, f.dev, f.disk
               FROM manual_rejects r
               JOIN files f ON f.id = r.file_id
              WHERE f.state = 'present'
                AND (?1 IS NULL OR EXISTS(SELECT 1 FROM family_members fm
                                           WHERE fm.file_id = f.id AND fm.family_id = ?1))
              ORDER BY f.path",
        )?;
        let rows = st
            .query_map([family], |r| {
                Ok(PlanRow {
                    family_id: 0,
                    file_id: r.get(0)?,
                    role: "unknown".into(),
                    path: r.get(1)?,
                    size: r.get(2)?,
                    width: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    height: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                    mtime: r.get(5)?,
                    inode: r.get(6)?,
                    dev: r.get(7)?,
                    disk: r.get(8)?,
                    pixel_hash: None,
                    content_hash: None,
                    is_keeper: false,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn plan_rows(&self) -> Result<Vec<PlanRow>> {
        self.plan_rows_scoped(None, None, None)
    }

    /// The rows a plan is built from, narrowed to one group or one folder.
    ///
    /// Acting on a single group used to read every member of every group in
    /// the archive — sixty thousand rows to decide about two files, twice
    /// over, because the run re-checks the plan it was given. The narrowing
    /// belongs here, where the rows are fetched.
    ///
    /// The folder is matched by prefix, which is as far as SQL can take it;
    /// the caller still decides what counts as being *in* that folder.
    pub fn plan_rows_scoped(
        &self,
        family: Option<i64>,
        folder_prefix: Option<&str>,
        keeper_folder_prefix: Option<&str>,
    ) -> Result<Vec<PlanRow>> {
        let mut st = self.conn.prepare(
            "SELECT fm.family_id, fm.file_id, fm.role, f.path, f.size, f.width, f.height,
                    f.mtime, f.inode, f.dev, f.disk, f.pixel_hash,
                    fa.keeper_file, f.content_hash
               FROM family_members fm
               JOIN files f    ON f.id = fm.file_id
               JOIN families fa ON fa.id = fm.family_id
              WHERE f.state = 'present'
                AND (?1 IS NULL OR fm.family_id = ?1)
                AND (?2 IS NULL OR fm.family_id IN (
                      SELECT x.family_id
                        FROM family_members x JOIN files xf ON xf.id = x.file_id
                       WHERE xf.state = 'present' AND xf.path LIKE ?2 || '%'))
                AND (?3 IS NULL OR fa.keeper_file IN (
                      SELECT kf.id FROM files kf WHERE kf.path LIKE ?3 || '%'))
              ORDER BY fm.family_id",
        )?;
        let rows = st
            .query_map(
                rusqlite::params![family, folder_prefix, keeper_folder_prefix],
                |r| {
                    let file_id: i64 = r.get(1)?;
                    Ok(PlanRow {
                        family_id: r.get(0)?,
                        file_id,
                        role: r.get(2)?,
                        path: r.get(3)?,
                        size: r.get(4)?,
                        width: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                        height: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                        mtime: r.get(7)?,
                        inode: r.get(8)?,
                        dev: r.get(9)?,
                        disk: r.get(10)?,
                        pixel_hash: r.get(11)?,
                        content_hash: r.get(13)?,
                        is_keeper: r.get::<_, Option<i64>>(12)? == Some(file_id),
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[derive(Debug, Clone)]
pub struct PlanRow {
    pub family_id: i64,
    pub file_id: i64,
    pub role: String,
    pub path: String,
    pub size: i64,
    pub width: i64,
    pub height: i64,
    pub mtime: i64,
    pub inode: i64,
    pub dev: i64,
    pub disk: String,
    pub pixel_hash: Option<Vec<u8>>,
    pub content_hash: Option<Vec<u8>>,
    pub is_keeper: bool,
}

impl Db {
    pub fn clear_series(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM series_members; DELETE FROM series;")?;
        Ok(())
    }

    pub fn insert_series(
        &self,
        kind: &str,
        started_at: Option<i64>,
        camera: Option<&str>,
        best: Option<i64>,
        protected: bool,
        run_id: i64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO series(kind, started_at, camera, best_file, protected, built_run)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![kind, started_at, camera, best, protected as i64, run_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn insert_series_member(
        &self,
        series_id: i64,
        file_id: i64,
        rank: i64,
        score: f64,
        breakdown: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO series_members(series_id, file_id, rank, score, breakdown)
             VALUES (?1,?2,?3,?4,?5)",
            params![series_id, file_id, rank, score, breakdown],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SeriesMemberRow {
    pub file_id: i64,
    pub name: String,
    pub path: String,
    pub rank: i64,
    pub score: f64,
    pub breakdown: String,
    pub sharpness: Option<f64>,
    pub thumb_key: Option<String>,
    pub taken_at: Option<i64>,
    pub is_best: bool,
    /// The user looked at this frame and did not want it.
    pub is_rejected: bool,
    /// The group of copies this frame belongs to, when it has one. A burst
    /// of fifteen frames is often three photographs and twelve copies, and
    /// reading it as fifteen separate frames is how the copies get kept.
    pub family_id: Option<i64>,
    /// How many present files that group holds, this one included.
    pub family_size: i64,
    /// Whether this is the file the group would keep.
    pub is_family_keeper: bool,
}

#[derive(Debug, Clone)]
pub struct SeriesRow {
    pub id: i64,
    pub kind: String,
    pub started_at: Option<i64>,
    pub camera: Option<String>,
    pub protected: bool,
    pub members: Vec<SeriesMemberRow>,
}

impl Db {
    pub fn series_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM series", [], |r| r.get(0))?)
    }

    pub fn series_list(&self, limit: i64, offset: i64) -> Result<Vec<SeriesRow>> {
        let mut st = self.conn.prepare(
            "SELECT id FROM series ORDER BY (
                 SELECT COUNT(*) FROM series_members m WHERE m.series_id = series.id
             ) DESC, started_at LIMIT ?1 OFFSET ?2",
        )?;
        let ids = st
            .query_map(params![limit, offset], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(st);
        let mut out = Vec::new();
        for id in ids {
            if let Some(s) = self.series(id)? {
                out.push(s);
            }
        }
        Ok(out)
    }

    pub fn series(&self, id: i64) -> Result<Option<SeriesRow>> {
        let head = self
            .conn
            .query_row(
                "SELECT id, kind, started_at, camera, protected, best_file
                   FROM series WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, i64>(4)? != 0,
                        r.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, kind, started_at, camera, protected, best)) = head else {
            return Ok(None);
        };

        let mut st = self.conn.prepare(
            "SELECT sm.file_id, f.name, f.path, sm.rank, sm.score, sm.breakdown,
                    f.sharpness, f.thumb_key, m.taken_at, r.file_id IS NOT NULL,
                    fm.family_id,
                    COALESCE((SELECT COUNT(*) FROM family_members x
                                JOIN files xf ON xf.id = x.file_id
                               WHERE x.family_id = fm.family_id
                                 AND xf.state = 'present'), 0),
                    fa.keeper_file IS NOT NULL AND fa.keeper_file = sm.file_id
               FROM series_members sm
               JOIN files f ON f.id = sm.file_id
               LEFT JOIN meta m ON m.file_id = f.id
               LEFT JOIN manual_rejects r ON r.file_id = f.id
               LEFT JOIN family_members fm ON fm.file_id = sm.file_id
               LEFT JOIN families fa ON fa.id = fm.family_id
              WHERE sm.series_id = ?1
              -- Shooting order, not quality order. A burst read out of
              -- sequence makes the subject jump back and forth, and the one
              -- frame the user is looking for could be anywhere. The rank
              -- still travels with each row; it is a label, not an order.
              -- Cameras stamp a whole second, so several frames share a
              -- timestamp and the file name is what separates them.
              ORDER BY COALESCE(m.taken_at, f.mtime), f.name, f.id",
        )?;
        let members = st
            .query_map(params![id], |r| {
                let file_id: i64 = r.get(0)?;
                Ok(SeriesMemberRow {
                    file_id,
                    name: r.get(1)?,
                    path: r.get(2)?,
                    rank: r.get(3)?,
                    score: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    breakdown: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    sharpness: r.get(6)?,
                    thumb_key: r.get(7)?,
                    taken_at: r.get(8)?,
                    is_best: Some(file_id) == best,
                    is_rejected: r.get(9)?,
                    family_id: r.get(10)?,
                    family_size: r.get(11)?,
                    is_family_keeper: r.get(12)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(Some(SeriesRow {
            id,
            kind,
            started_at,
            camera,
            protected,
            members,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct CategoryCount {
    pub category: String,
    pub count: i64,
    pub bytes: i64,
}

impl Db {
    /// Store a verdict, leaving any the user set by hand alone.
    pub fn set_category(
        &self,
        file_id: i64,
        category: &str,
        confidence: f64,
        evidence: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_categories(file_id, category, confidence, evidence, manual)
             VALUES (?1,?2,?3,?4,0)
             ON CONFLICT(file_id) DO UPDATE SET
                 category=excluded.category, confidence=excluded.confidence,
                 evidence=excluded.evidence
               WHERE file_categories.manual = 0",
            params![file_id, category, confidence, evidence],
        )?;
        Ok(())
    }

    pub fn set_category_manual(&self, file_id: i64, category: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO file_categories(file_id, category, confidence, evidence, manual)
             VALUES (?1,?2,1.0,'указано вручную',1)
             ON CONFLICT(file_id) DO UPDATE SET
                 category=excluded.category, confidence=1.0,
                 evidence=excluded.evidence, manual=1",
            params![file_id, category],
        )?;
        Ok(())
    }

    pub fn category_counts(&self) -> Result<Vec<CategoryCount>> {
        let mut st = self.conn.prepare(
            "SELECT c.category, COUNT(*), COALESCE(SUM(f.size), 0)
               FROM file_categories c JOIN files f ON f.id = c.file_id
              WHERE f.state = 'present'
              GROUP BY c.category ORDER BY 2 DESC",
        )?;
        let rows = st
            .query_map([], |r| {
                Ok(CategoryCount {
                    category: r.get(0)?,
                    count: r.get(1)?,
                    bytes: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn files_in_category(&self, category: &str, limit: i64) -> Result<Vec<MemberRow>> {
        let mut st = self.conn.prepare(
            "SELECT f.id, f.path, f.name, c.category, f.size, f.width, f.height,
                    f.container, c.confidence, c.evidence, NULL, f.thumb_key
               FROM file_categories c JOIN files f ON f.id = c.file_id
              WHERE c.category = ?1 AND f.state = 'present'
              ORDER BY c.confidence DESC, f.size DESC LIMIT ?2",
        )?;
        let rows = st
            .query_map(params![category, limit], |r| {
                Ok(MemberRow {
                    file_id: r.get(0)?,
                    path: r.get(1)?,
                    name: r.get(2)?,
                    role: r.get(3)?,
                    size: r.get(4)?,
                    width: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    height: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    container: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    quality: r.get::<_, Option<f64>>(8)?.unwrap_or(0.0),
                    breakdown: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    evidence: None,
                    thumb_key: r.get(11)?,
                    pixel_hash: None,
                    content_hash: None,
                    is_rejected: false,
                    is_keeper: false,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}
