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
use std::time::{Duration, Instant};

use crate::permit::DiskPermits;
use crate::priority;
use pc_image::read;

/// How long an open write transaction may hold rows back from readers.
///
/// Indexing streams its results into the database as it goes, so the web
/// interface can show families forming while the pass is still running. That
/// only works if the transaction is closed often enough for another
/// connection to see the rows; every couple of seconds is frequent enough to
/// watch and rare enough that the commits cost nothing.
const COMMIT_EVERY: Duration = Duration::from_secs(2);

/// Rows buffered between the decoders and the single writer.
///
/// Unbounded, a fast disk lets the decoders run thousands of thumbnails ahead
/// of the writer and hold them all in memory. The bound is per worker, which
/// keeps every core busy without letting the queue grow without limit.
const QUEUE_PER_WORKER: usize = 8;

pub struct Options {
    pub min_file_size: u64,
    pub readers_per_disk: usize,
    /// Re-read files that are already current.
    pub reindex: bool,
    /// Decoding threads. Zero means "leave one core free".
    pub workers: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_file_size: 100 * 1024,
            readers_per_disk: 2,
            reindex: false,
            workers: 0,
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
    /// Files indexed whose thumbnail could not be stored, and the first
    /// such file with the reason. Each is also a refusal on the job.
    pub thumbs_failed: u64,
    pub thumb_failure: Option<(String, String)>,
}

/// One file's result, ready for the single writer.
struct Indexed {
    file: NewFile,
    meta: Option<NewMeta>,
    /// The thumbnail could not be stored (el-5x1uh O2). A bound cache's
    /// refusal ([`pc_core::storage::NotBound`]) stops the job; anything
    /// else is a refusal on this file, which is still indexed.
    thumb_error: Option<anyhow::Error>,
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
                thumb_error: None,
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
                thumb_error: None,
            }
        }
    };

    let gray = &probe.thumb.gray;
    let crops = pc_hash::crop_hashes(gray);
    let mut crop_bytes = Vec::with_capacity(40);
    for c in crops {
        crop_bytes.extend_from_slice(&c.to_le_bytes());
    }

    let (thumb_key, thumb_error) = match store.put(&probe.thumb.jpeg) {
        Ok(key) => (Some(key), None),
        Err(e) => (None, Some(e)),
    };
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
            content_hash: Some(probe.content_hash.to_vec()),
            phash_canon: Some(bits_to_i64(pc_hash::canonical_phash(gray))),
            phash: Some(bits_to_i64(pc_hash::phash(gray))),
            dhash: Some(bits_to_i64(pc_hash::dhash(gray))),
            phash_crops: Some(crop_bytes),
            thumb_key,
            sharpness: Some(probe.metrics.sharpness as f64),
            clip_low: Some(probe.metrics.clip_low as f64),
            clip_high: Some(probe.metrics.clip_high as f64),
            entropy: Some(probe.metrics.entropy as f64),
            contrast: Some(probe.metrics.contrast as f64),
            saturation: Some(probe.metrics.saturation as f64),
            chroma: Some(probe.metrics.chroma as f64),
            tonal_range: Some(probe.metrics.tonal_range as f64),
            white_fraction: Some(probe.metrics.white_fraction as f64),
            bimodality: Some(probe.metrics.bimodality as f64),
            text_rows: Some(probe.metrics.text_rows as f64),
            text_banding: Some(probe.metrics.text_banding as f64),
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
        thumb_error,
    }
}

pub fn run(db: &Db, roots: &[PathBuf], store: &ThumbStore, opts: &Options) -> Result<Summary> {
    run_controlled(db, roots, store, opts, &pc_core::work::Control::default())
}

pub fn run_controlled(
    db: &Db,
    roots: &[PathBuf],
    store: &ThumbStore,
    opts: &Options,
    control: &pc_core::work::Control,
) -> Result<Summary> {
    let started = Instant::now();
    let run_id = db.start_run(
        &roots
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>(),
        env!("CARGO_PKG_VERSION"),
    )?;

    let walk = pc_walk::scan_controlled(
        roots,
        &pc_walk::Options {
            min_file_size: opts.min_file_size,
            collect_files: true,
        },
        control,
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

    control.begin(
        pc_core::tr!("Чтение изображений", "Reading images"),
        todo.len() as u64,
        todo.iter().map(|f| f.size).sum(),
    )?;
    {
        let mut p = control.progress.lock().unwrap();
        p.note = pc_core::tr!(
            "RAW читаются через встроенное превью",
            "Raw files are read through their embedded preview"
        )
        .into();
        let mut disks = std::collections::BTreeMap::<String, u64>::new();
        for f in &todo {
            *disks.entry(f.disk.label.clone()).or_default() += 1;
        }
        p.per_disk = disks
            .into_iter()
            .map(|(disk, total)| pc_core::work::DiskProgress {
                disk,
                total,
                done: 0,
            })
            .collect();
    }
    let permits = DiskPermits::new(devs, opts.readers_per_disk);
    let bytes_read = AtomicU64::new(0);
    let workers = if opts.workers == 0 {
        priority::default_workers()
    } else {
        opts.workers
    };
    let (tx, rx) = mpsc::sync_channel::<Indexed>(workers * QUEUE_PER_WORKER);
    let total = todo.len() as u64;
    let pool = priority::pool(workers);
    // Set when a bound cache refused a thumbnail: every later one would be
    // refused too, so the readers stop and the writer records nothing more.
    let halted = std::sync::atomic::AtomicBool::new(false);
    let mut halt: Option<anyhow::Error> = None;
    tracing::info!(workers, "декодирование в фоновом классе планировщика");

    // SQLite takes one writer, so the workers hand results over a channel and
    // the main thread commits them in batches.
    std::thread::scope(|scope| -> Result<()> {
        scope.spawn(|| {
            pool.install(|| {
                todo.par_iter().for_each_with(tx, |tx, hit| {
                    if control.check().is_ok() && !halted.load(Ordering::Relaxed) {
                        let _ = tx.send(process(hit, &permits, store, &bytes_read));
                    }
                });
            });
        });
        // The writer is part of the same batch job and yields to the desktop
        // for the same reason its readers do.
        priority::background_thread();

        let mut done = 0u64;
        let mut last_report = Instant::now();
        let mut last_commit = Instant::now();
        db.conn.execute_batch("BEGIN")?;
        for item in rx {
            if control.check().is_err() || halt.is_some() {
                continue;
            }
            control.current(&item.file.path)?;
            if let Some(e) = &item.thumb_error {
                if e.downcast_ref::<pc_core::storage::NotBound>().is_some() {
                    // Not recorded: a file written now would count as
                    // current and never get its thumbnail.
                    control.refuse(&item.file.path, &format!("{e:#}"));
                    halted.store(true, Ordering::Relaxed);
                    halt = item.thumb_error;
                    continue;
                }
                let why = pc_core::tf!(
                    "миниатюра не сохранена: {0}",
                    "the thumbnail was not stored: {0}",
                    format!("{e:#}")
                );
                control.refuse(&item.file.path, &why);
                summary.thumbs_failed += 1;
                summary
                    .thumb_failure
                    .get_or_insert_with(|| (item.file.path.clone(), why));
            }
            let id = db.upsert_file(&item.file, run_id)?;
            if let Some(m) = &item.meta {
                db.upsert_meta(id, m)?;
            }
            if let Some(reason) = &item.file.skipped_reason {
                control.refuse(&item.file.path, reason);
                summary.skipped += 1;
            } else {
                summary.indexed += 1;
            }

            control.advance(item.file.size as u64, Some(&item.file.disk));
            done += 1;
            if last_commit.elapsed() >= COMMIT_EVERY {
                db.conn.execute_batch("COMMIT; BEGIN")?;
                last_commit = Instant::now();
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

    if let Some(refusal) = halt {
        return Err(refusal.context(pc_core::tr!(
            "индексация остановлена: кэш миниатюр отклонён",
            "indexing stopped: the thumbnail cache was refused"
        )));
    }
    control.check()?;
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
            "Indexed {} of {} files, skipped {}, already current {}.\n\
             Read {} of {} on disk — {saved:.0}% saved by embedded previews.\n\
             Time: {:.1} s{}",
            self.indexed,
            self.seen,
            self.skipped,
            self.already_current,
            fmt_bytes(self.bytes_read),
            fmt_bytes(self.bytes_on_disk),
            self.elapsed_secs,
            match &self.thumb_failure {
                Some((path, why)) => format!(
                    "\nThumbnails not stored: {} (first: {path}: {why})",
                    self.thumbs_failed
                ),
                None => String::new(),
            }
        )
    }
}

/// A thumbnail that could not be stored is seen, not swallowed
/// (el-5x1uh O2): a bound cache's refusal stops the job with that refusal,
/// any other failure is a refusal on the file and a line in the summary.
#[cfg(test)]
mod thumbnail_refusal_tests {
    use super::*;
    use pc_core::storage::{NotBound, StorageBinding};
    use std::path::Path;
    use std::sync::Arc;

    /// Seven different real JPEGs, so seven different thumbnails.
    fn archive(root: &Path) -> PathBuf {
        let archive = root.canonicalize().unwrap().join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        for n in 0..7u32 {
            let img = image::RgbImage::from_fn(320, 240, |x, y| {
                image::Rgb([
                    ((x * (n + 3) + y * 7) % 256) as u8,
                    ((y * (n + 5)) % 256) as u8,
                    ((x + y + n * 31) % 256) as u8,
                ])
            });
            let mut encoded = Vec::new();
            image::codecs::jpeg::JpegEncoder::new(&mut encoded)
                .encode_image(&img)
                .unwrap();
            std::fs::write(archive.join(format!("IMG_{:04}.JPG", 4100 + n)), &encoded).unwrap();
        }
        archive
    }

    fn options() -> Options {
        Options {
            min_file_size: 0,
            readers_per_disk: 1,
            reindex: false,
            workers: 2,
        }
    }

    fn files(db: &Db) -> i64 {
        db.conn
            .query_row("SELECT count(*) FROM files", [], |r| r.get(0))
            .unwrap()
    }

    #[derive(Debug)]
    struct Cache {
        refuse: bool,
    }

    impl StorageBinding for Cache {
        fn check_database(&self) -> std::result::Result<(), String> {
            Ok(())
        }
        fn check_thumbnails(&self) -> std::result::Result<(), String> {
            if self.refuse {
                Err("the thumbnail folder was replaced (5531)".into())
            } else {
                Ok(())
            }
        }
        fn check_thumbnail_folder(&self, _: &std::fs::File) -> std::result::Result<(), String> {
            self.check_thumbnails()
        }
    }

    #[test]
    fn a_refused_bound_cache_stops_the_index_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = archive(tmp.path());
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        let thumbs = tmp.path().join("thumbs");
        std::fs::create_dir(&thumbs).unwrap();
        let refused = ThumbStore::bound(&thumbs, Arc::new(Cache { refuse: true }));
        let control = pc_core::work::Control::default();

        let err = run_controlled(
            &db,
            std::slice::from_ref(&archive),
            &refused,
            &options(),
            &control,
        )
        .expect_err("the index finished as if the thumbnails were stored");
        assert!(err.downcast_ref::<NotBound>().is_some(), "{err:#}");
        assert!(format!("{err:#}").contains("5531"), "{err:#}");
        let refusals = control.progress.lock().unwrap().refusals.clone();
        assert!(!refusals.is_empty(), "no refusal in the job's status");
        assert!(
            refusals.iter().all(|(_, why)| why.contains("5531")),
            "{refusals:?}"
        );
        assert!(refusals[0].0.contains("IMG_41"), "{refusals:?}");
        assert_eq!(files(&db), 0, "files recorded without their thumbnails");
        assert_eq!(std::fs::read_dir(&thumbs).unwrap().count(), 0);

        // With the cache confirmed again, the same files are indexed — none
        // was recorded as current without its thumbnail.
        let fine = ThumbStore::bound(&thumbs, Arc::new(Cache { refuse: false }));
        let summary = run(&db, &[archive], &fine, &options()).unwrap();
        assert_eq!((summary.indexed, summary.already_current), (7, 0));
        assert_eq!(summary.thumbs_failed, 0);
        assert_eq!(files(&db), 7);
    }

    #[test]
    fn a_thumbnail_that_cannot_be_stored_is_a_refusal_and_a_summary_line() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = archive(tmp.path());
        let db = Db::open(&tmp.path().join("pc.db")).unwrap();
        // A file where the cache folder should be: nothing can be stored.
        let thumbs = tmp.path().join("thumbs");
        std::fs::write(&thumbs, b"not a folder 8840").unwrap();
        let store = ThumbStore::new(&thumbs);
        let control = pc_core::work::Control::default();

        let summary = run_controlled(&db, &[archive], &store, &options(), &control).unwrap();
        assert_eq!(summary.indexed, 7);
        assert_eq!(summary.thumbs_failed, 7);
        let report = summary.report();
        assert!(report.contains("Thumbnails not stored: 7"), "{report}");
        let refusals = control.progress.lock().unwrap().refusals.clone();
        assert_eq!(refusals.len(), 7, "{refusals:?}");
        assert!(
            refusals.iter().all(|(_, why)| why.contains("thumbnail")),
            "{refusals:?}"
        );
        assert_eq!(std::fs::read(&thumbs).unwrap(), b"not a folder 8840");
    }
}
