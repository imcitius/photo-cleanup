//! Making the thumbnails again for frames whose thumbnail is no good.
//!
//! Indexing writes a thumbnail once and never returns to it: a file that is
//! unchanged on disk is a file that is done. That is right for the archive
//! and wrong for a thumbnail that came out grey — the picture is fine, the
//! small copy of it is not, and nothing short of touching every file would
//! ever try again.
//!
//! So this pass reads the thumbnails rather than the archive. One that is
//! missing, unreadable or flat — a single tone across the whole square, which
//! is what a grey tile in the interface is — is made again from the original.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use pc_core::work::Control;
use pc_core::ThumbStore;
use pc_db::Db;
use rayon::prelude::*;

/// Below this the square holds one tone and no picture.
const FLAT: f32 = 8.0;

#[derive(Debug, Default)]
pub struct Report {
    pub checked: u64,
    pub rebuilt: u64,
    /// Made again and still flat: the fault is in reading the file, not in
    /// the thumbnail that was stored.
    pub still_flat: u64,
    pub failed: u64,
}

impl Report {
    pub fn describe(&self) -> String {
        pc_core::tf!(
            "Проверено {0}, перестроено {1}, осталось плоскими {2}, не прочитано {3}.",
            "Checked {0}, rebuilt {1}, still flat {2}, unreadable {3}.",
            self.checked,
            self.rebuilt,
            self.still_flat,
            self.failed
        )
    }
}

struct Row {
    id: i64,
    path: String,
    thumb_key: Option<String>,
}

/// Everything one re-read of a file yields. The thumbnail is the visible
/// part; the hashes and the measurements matter more, because groups, bursts
/// and kinds are all decided by them.
struct Fixed {
    id: i64,
    key: String,
    pixel_hash: Vec<u8>,
    phash: i64,
    dhash: i64,
    crops: Vec<u8>,
    metrics: pc_image::metrics::Metrics,
    width: i64,
    height: i64,
}

fn bits_to_i64(v: u64) -> i64 {
    i64::from_le_bytes(v.to_le_bytes())
}

/// Whether the stored thumbnail is worth keeping.
fn is_good(store: &ThumbStore, key: Option<&String>) -> bool {
    let Some(key) = key else { return false };
    let Some(bytes) = store.get(key) else {
        return false;
    };
    if bytes.is_empty() {
        return false;
    }
    match image::load_from_memory(&bytes) {
        Ok(img) => pc_image::metrics::measure(&img).tonal_range >= FLAT,
        Err(_) => false,
    }
}

/// Rebuild what needs it. `all` ignores the state of the stored thumbnail and
/// makes every one again, which is the answer when the fault was in the
/// making rather than in the storing.
pub fn rebuild(
    db: &Db,
    store: &ThumbStore,
    all: bool,
    container: Option<&str>,
    control: &Control,
) -> Result<Report> {
    let rows: Vec<Row> = {
        // One container at a time, for when a fault is known to belong to a
        // format: re-reading four hundred TIFFs is a minute, re-reading sixty
        // thousand photographs is an evening.
        let mut st = db.conn.prepare(
            "SELECT id, path, thumb_key FROM files
              WHERE state = 'present' AND phash IS NOT NULL
                AND (?1 IS NULL OR container = ?1)
              ORDER BY id",
        )?;
        let rows = st
            .query_map([container], |r| {
                Ok(Row {
                    id: r.get(0)?,
                    path: r.get(1)?,
                    thumb_key: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    control.begin(
        pc_core::tr!("Перестройка миниатюр", "Making the thumbnails again"),
        rows.len() as u64,
        0,
    )?;

    let checked = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let still_flat = AtomicU64::new(0);
    // (file id, key) for the ones that were made again.
    let made: Vec<Fixed> = rows
        .par_iter()
        .filter_map(|row| {
            if control.check().is_err() {
                return None;
            }
            checked.fetch_add(1, Ordering::Relaxed);
            control.advance(0, None);
            if !all && is_good(store, row.thumb_key.as_ref()) {
                return None;
            }
            let path = std::path::Path::new(&row.path);
            let size = std::fs::metadata(path).ok()?.len();
            let read = match pc_image::read_for_probe(path, size) {
                Ok(r) => r,
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            };
            let name = pc_core::file_name_str(path).to_string();
            let probe =
                match pc_image::probe_parts(path, &read.head, read.preview.as_deref(), &name) {
                    Ok(p) => p,
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                };
            if probe.metrics.tonal_range < FLAT {
                still_flat.fetch_add(1, Ordering::Relaxed);
            }
            let key = match store.put(&probe.thumb.jpeg) {
                Ok(key) => key,
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
            };
            let gray = &probe.thumb.gray;
            let mut crops = Vec::with_capacity(40);
            for c in pc_hash::crop_hashes(gray) {
                crops.extend_from_slice(&c.to_le_bytes());
            }
            Some(Fixed {
                id: row.id,
                key,
                pixel_hash: pc_hash::pixel_hash(gray).to_vec(),
                phash: bits_to_i64(pc_hash::phash(gray)),
                dhash: bits_to_i64(pc_hash::dhash(gray)),
                crops,
                metrics: probe.metrics,
                width: probe.width as i64,
                height: probe.height as i64,
            })
        })
        .collect();

    // The hashes and the measurements are written too. They came from the
    // same decode as the thumbnail, so a thumbnail that was wrong means a
    // phash that was wrong — and groups and bursts are decided by the phash.
    // Re-reading the archive once should settle all of it.
    db.conn.execute_batch("BEGIN")?;
    {
        let mut up = db.conn.prepare(
            "UPDATE files SET thumb_key = ?2, pixel_hash = ?3, phash = ?4, dhash = ?5,
                              phash_crops = ?6, width = ?7, height = ?8,
                              sharpness = ?9, clip_low = ?10, clip_high = ?11, entropy = ?12,
                              contrast = ?13, saturation = ?14, chroma = ?15, tonal_range = ?16,
                              white_fraction = ?17, bimodality = ?18, text_rows = ?19,
                              text_banding = ?20
              WHERE id = ?1",
        )?;
        for f in &made {
            let m = &f.metrics;
            up.execute(rusqlite::params![
                f.id,
                f.key,
                f.pixel_hash,
                f.phash,
                f.dhash,
                f.crops,
                f.width,
                f.height,
                m.sharpness as f64,
                m.clip_low as f64,
                m.clip_high as f64,
                m.entropy as f64,
                m.contrast as f64,
                m.saturation as f64,
                m.chroma as f64,
                m.tonal_range as f64,
                m.white_fraction as f64,
                m.bimodality as f64,
                m.text_rows as f64,
                m.text_banding as f64,
            ])?;
        }
    }
    db.conn.execute_batch("COMMIT")?;

    Ok(Report {
        checked: checked.load(Ordering::Relaxed),
        rebuilt: made.len() as u64,
        still_flat: still_flat.load(Ordering::Relaxed),
        failed: failed.load(Ordering::Relaxed),
    })
}
