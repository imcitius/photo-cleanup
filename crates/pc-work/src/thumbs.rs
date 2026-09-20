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
pub fn rebuild(db: &Db, store: &ThumbStore, all: bool, control: &Control) -> Result<Report> {
    let rows: Vec<Row> = {
        let mut st = db.conn.prepare(
            "SELECT id, path, thumb_key FROM files
              WHERE state = 'present' AND phash IS NOT NULL
              ORDER BY id",
        )?;
        let rows = st
            .query_map([], |r| {
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
    let made: Vec<(i64, String)> = rows
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
            match store.put(&probe.thumb.jpeg) {
                Ok(key) => Some((row.id, key)),
                Err(_) => {
                    failed.fetch_add(1, Ordering::Relaxed);
                    None
                }
            }
        })
        .collect();

    db.conn.execute_batch("BEGIN")?;
    {
        let mut up = db
            .conn
            .prepare("UPDATE files SET thumb_key = ?2 WHERE id = ?1")?;
        for (id, key) in &made {
            up.execute(rusqlite::params![id, key])?;
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
