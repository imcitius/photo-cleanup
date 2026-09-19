//! `index` — read every image once and record what later stages need.
//!
//! Reading and decoding want opposite things from parallelism. A spinning
//! disk wants one or two readers so the head travels forward; twelve cores
//! want twelve decodes in flight. So reads take a per-disk permit and release
//! it before any pixels are touched.

use anyhow::Result;
use pc_core::{fmt_bytes, ThumbStore};
use pc_db::{Db, NewFile, NewMeta};
use rayon::prelude::*;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use crate::permit::DiskPermits;
use pc_image::read;

pub struct Options {
    pub min_file_size: u64,
    pub readers_per_disk: usize,
    /// Re-read files that are already current.
    pub reindex: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_file_size: 100 * 1024,
            readers_per_disk: 2,
            reindex: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub seen: u64,
    pub indexed: u64,
    pub skipped: u64,
    pub already_current: u64,
    pub bytes_read: u64,
    pub bytes_on_disk: u64,
    pub elapsed_secs: f64,
}

/// One file's result, ready for the single writer.
struct Indexed {
    file: NewFile,
    meta: Option<NewMeta>,
}

fn u64_to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn bits_to_i64(v: u64) -> i64 {
    i64::from_le_bytes(v.to_le_bytes())
}

fn process(
    hit: &pc_walk::FileHit,
    permits: &DiskPermits,
    store: &ThumbStore,
    bytes_read: &AtomicU64,
) -> Indexed {
    let base = NewFile {
        path: hit.path.display().to_string(),
        name: hit.name.clone(),
        disk: hit.disk.label.clone(),
        dev: u64_to_i64(hit.disk.dev),
        inode: u64_to_i64(hit.inode),
        nlink: u64_to_i64(hit.nlink),
        size: u64_to_i64(hit.size),
        mtime: hit.mtime,
        ..Default::default()
    };

    // The read holds a disk permit; everything after it does not.
    let read = {
        let _permit = permits.get(hit.disk.dev).map(|s| s.acquire());
        read::read_for_probe(&hit.path, hit.size)
    };

    let r = match read {
        Ok(r) => r,
        Err(e) => {
            return Indexed {
                file: NewFile {
                    skipped_reason: Some(e.to_string()),
                    ..base
                },
                meta: None,
            }
        }
    };
    bytes_read.fetch_add(r.bytes_read, Ordering::Relaxed);

    let probe = match pc_image::probe_parts(&hit.path, &r.head, r.preview.as_deref(), &hit.name) {
        Ok(p) => p,
        Err(e) => {
            return Indexed {
                file: NewFile {
                    container: Some(r.container.as_str().to_string()),
                    partial_hash: Some(r.partial_hash.to_vec()),
                    skipped_reason: Some(e.to_string()),
                    ..base
                },
                meta: None,
            }
        }
    };

    let gray = &probe.thumb.gray;
    let crops = pc_hash::crop_hashes(gray);
    let mut crop_bytes = Vec::with_capacity(40);
    for c in crops {
        crop_bytes.extend_from_slice(&c.to_le_bytes());
    }

    let thumb_key = store.put(&probe.thumb.jpeg).ok();
    let m = &probe.meta;

    Indexed {
        file: NewFile {
            container: Some(probe.container.as_str().to_string()),
            extension_lied: probe.extension_lied,
            width: Some(probe.width as i64),
            height: Some(probe.height as i64),
            orientation: Some(m.orientation as i64),
            pixel_source: Some(probe.source.as_str().to_string()),
            partial_hash: Some(r.partial_hash.to_vec()),
            pixel_hash: Some(pc_hash::pixel_hash(gray).to_vec()),
            phash: Some(bits_to_i64(pc_hash::phash(gray))),
            dhash: Some(bits_to_i64(pc_hash::dhash(gray))),
            phash_crops: Some(crop_bytes),
            thumb_key,
            sharpness: Some(probe.metrics.sharpness as f64),
            clip_low: Some(probe.metrics.clip_low as f64),
            clip_high: Some(probe.metrics.clip_high as f64),
            entropy: Some(probe.metrics.entropy as f64),
            contrast: Some(probe.metrics.contrast as f64),
            ..base
        },
        meta: Some(NewMeta {
            taken_at: m
                .taken_at
                .or_else(|| pc_image::meta::date_from_name(&hit.name)),
            date_source: Some(
                if m.taken_at.is_some() {
                    m.date_source
                } else if pc_image::meta::date_from_name(&hit.name).is_some() {
                    pc_image::meta::DateSource::Filename
                } else {
                    pc_image::meta::DateSource::None
                }
                .as_str()
                .to_string(),
            ),
            camera_make: m.camera_make.clone(),
            camera_model: m.camera_model.clone(),
            body_serial: m.body_serial.clone(),
            lens: m.lens.clone(),
            iso: m.iso.map(|v| v as i64),
            f_number: m.f_number,
            focal_length: m.focal_length,
            exposure: m.exposure.clone(),
            gps_lat: m.gps.map(|(a, _)| a),
            gps_lon: m.gps.map(|(_, b)| b),
            software: m.software.clone(),
            xmp_document_id: m.provenance.document_id.clone(),
            xmp_original_id: m.provenance.original_document_id.clone(),
            xmp_derived_from: m.provenance.derived_from.clone(),
            dng_original_raw: m.provenance.dng_original_raw.clone(),
        }),
    }
}

pub fn run(db: &Db, roots: &[PathBuf], store: &ThumbStore, opts: &Options) -> Result<Summary> {
    let started = Instant::now();
    let run_id = db.start_run(
        &roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        env!("CARGO_PKG_VERSION"),
    )?;

    let walk = pc_walk::scan_with(
        roots,
        &pc_walk::Options {
            min_file_size: opts.min_file_size,
            collect_files: true,
        },
    )?;

    let mut summary = Summary {
        seen: walk.files.len() as u64,
        bytes_on_disk: walk.files.iter().map(|f| f.size).sum(),
        ..Default::default()
    };

    // Skip what is already current, so an interrupted run resumes.
    let mut todo = Vec::with_capacity(walk.files.len());
    for f in walk.files {
        let known = !opts.reindex
            && db.file_is_current(
                &f.path.display().to_string(),
                u64_to_i64(f.size),
                f.mtime,
                u64_to_i64(f.inode),
            )?;
        if known {
            summary.already_current += 1;
        } else {
            todo.push(f);
        }
    }

    let devs: BTreeSet<u64> = todo.iter().map(|f| f.disk.dev).collect();
    tracing::info!(
        files = todo.len(),
        disks = devs.len(),
        readers_per_disk = opts.readers_per_disk,
        "индексация"
    );
    if todo.is_empty() {
        db.finish_run(run_id)?;
        summary.elapsed_secs = started.elapsed().as_secs_f64();
        return Ok(summary);
    }

    let permits = DiskPermits::new(devs, opts.readers_per_disk);
    let bytes_read = AtomicU64::new(0);
    let (tx, rx) = mpsc::channel::<Indexed>();
    let total = todo.len() as u64;

    // SQLite takes one writer, so the workers hand results over a channel and
    // the main thread commits them in batches.
    std::thread::scope(|scope| -> Result<()> {
        scope.spawn(|| {
            todo.par_iter().for_each_with(tx, |tx, hit| {
                let _ = tx.send(process(hit, &permits, store, &bytes_read));
            });
        });

        let mut done = 0u64;
        let mut last_report = Instant::now();
        db.conn.execute_batch("BEGIN")?;
        for item in rx {
            let id = db.upsert_file(&item.file, run_id)?;
            if let Some(m) = &item.meta {
                db.upsert_meta(id, m)?;
            }
            if item.file.skipped_reason.is_some() {
                summary.skipped += 1;
            } else {
                summary.indexed += 1;
            }

            done += 1;
            if done.is_multiple_of(512) {
                db.conn.execute_batch("COMMIT; BEGIN")?;
            }
            if last_report.elapsed().as_secs() >= 2 {
                let pct = done as f64 * 100.0 / total as f64;
                tracing::info!(
                    "{done}/{total} ({pct:.0}%), прочитано {}",
                    fmt_bytes(bytes_read.load(Ordering::Relaxed))
                );
                last_report = Instant::now();
            }
        }
        db.conn.execute_batch("COMMIT")?;
        Ok(())
    })?;

    db.finish_run(run_id)?;
    summary.bytes_read = bytes_read.load(Ordering::Relaxed);
    summary.elapsed_secs = started.elapsed().as_secs_f64();
    Ok(summary)
}

impl Summary {
    pub fn report(&self) -> String {
        let saved = if self.bytes_on_disk > 0 {
            100.0 - (self.bytes_read as f64 * 100.0 / self.bytes_on_disk as f64)
        } else {
            0.0
        };
        format!(
            "Проиндексировано {} из {} файлов, пропущено {}, уже актуальны {}.\n\
             Прочитано {} из {} на диске — сэкономлено {saved:.0}% за счёт встроенных превью.\n\
             Время: {:.1} с",
            self.indexed,
            self.seen,
            self.skipped,
            self.already_current,
            fmt_bytes(self.bytes_read),
            fmt_bytes(self.bytes_on_disk),
            self.elapsed_secs
        )
    }
}
