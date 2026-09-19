//! Filesystem walk for phase 0.
//!
//! Only metadata is read. Derived-data bundles are recognised by directory
//! name, summarised in one pass and then *pruned*: we never index the files
//! inside them, which is what keeps 24k Lightroom preview files out of the
//! main tables.

pub mod classify;

pub use classify::{classify_dir, is_backup_path};

use anyhow::{Context, Result};
use pc_core::{DerivedKind, Disk, DiskMap};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct BundleHit {
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: DerivedKind,
    /// Path of the owning `.lrcat`, when the name implies one.
    pub owner_ref: Option<PathBuf>,
    pub file_count: u64,
    pub size: u64,
    pub newest_mtime: i64,
    pub disk: Disk,
}

#[derive(Debug, Clone)]
pub struct CatalogHit {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub is_backup: bool,
    pub is_locked: bool,
    pub disk: Disk,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub bundles: Vec<BundleHit>,
    pub catalogs: Vec<CatalogHit>,
    pub dirs_visited: u64,
    pub errors: Vec<String>,
}

impl ScanResult {
    fn merge(&mut self, other: ScanResult) {
        self.bundles.extend(other.bundles);
        self.catalogs.extend(other.catalogs);
        self.dirs_visited += other.dirs_visited;
        self.errors.extend(other.errors);
    }
}

/// Recursive size/count/mtime of a bundle directory. Symlinks are not followed.
fn dir_stats(root: &Path) -> (u64, u64, i64) {
    let mut count = 0u64;
    let mut size = 0u64;
    let mut newest = 0i64;
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let Ok(md) = entry.metadata() else { continue };
        if md.is_file() {
            count += 1;
            size += md.len();
        }
        newest = newest.max(pc_core::time::mtime_unix(&md));
    }
    (count, size, newest)
}

fn walk_dir(dir: &Path, disk: &Disk, out: &mut ScanResult) {
    out.dirs_visited += 1;

    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            out.errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };

    for entry in rd {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let name = pc_core::file_name_str(&path).to_string();
        if name.is_empty() {
            continue;
        }

        // Never follow symlinks: they invite loops and double counting.
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }

        if ft.is_dir() {
            if name == pc_core::QUARANTINE_DIR {
                continue;
            }
            if let Some((kind, owner_base)) = classify_dir(&name) {
                let (file_count, size, newest_mtime) = dir_stats(&path);
                let owner_ref = owner_base.map(|b| dir.join(format!("{b}.lrcat")));
                out.bundles.push(BundleHit {
                    path,
                    is_dir: true,
                    kind,
                    owner_ref,
                    file_count,
                    size,
                    newest_mtime,
                    disk: disk.clone(),
                });
                // Pruned: the bundle is the unit, its contents are not indexed.
                continue;
            }
            if pc_core::is_pruned_dir_name(&name) {
                continue;
            }
            walk_dir(&path, disk, out);
            continue;
        }

        if !ft.is_file() {
            continue;
        }

        if name.ends_with(".lrcat") {
            let md = entry.metadata().ok();
            let lock = path.with_file_name(format!("{name}.lock"));
            out.catalogs.push(CatalogHit {
                name: name.trim_end_matches(".lrcat").to_string(),
                is_backup: is_backup_path(&path),
                is_locked: lock.exists(),
                size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                path,
                disk: disk.clone(),
            });
            continue;
        }

        if pc_core::is_system_junk_name(&name) {
            let md = entry.metadata().ok();
            out.bundles.push(BundleHit {
                path,
                is_dir: false,
                kind: DerivedKind::SystemJunk,
                owner_ref: None,
                file_count: 1,
                size: md.as_ref().map(|m| m.len()).unwrap_or(0),
                newest_mtime: md.as_ref().map(pc_core::time::mtime_unix).unwrap_or(0),
                disk: disk.clone(),
            });
        }
    }
}

/// Walk every root, sharding work across physical disks.
///
/// An Unraid array is not striped, so parallelism pays off across spindles but
/// hurts within one: each disk gets a single sequential walker.
pub fn scan(roots: &[PathBuf]) -> Result<ScanResult> {
    let mut map = DiskMap::new();
    let mut by_disk: BTreeMap<u64, (Disk, Vec<PathBuf>)> = BTreeMap::new();

    for root in roots {
        let canonical = root
            .canonicalize()
            .with_context(|| format!("корень недоступен: {}", root.display()))?;
        let disk = map.resolve(&canonical)?;
        by_disk
            .entry(disk.dev)
            .or_insert_with(|| (disk.clone(), Vec::new()))
            .1
            .push(canonical);
    }

    let merged = Mutex::new(ScanResult::default());
    by_disk.into_par_iter().for_each(|(_dev, (disk, paths))| {
        let mut local = ScanResult::default();
        for p in &paths {
            tracing::info!(disk = %disk.label, root = %p.display(), "обход");
            walk_dir(p, &disk, &mut local);
        }
        merged.lock().unwrap().merge(local);
    });

    let mut out = merged.into_inner().unwrap();
    out.bundles.sort_by_key(|b| std::cmp::Reverse(b.size));
    out.catalogs.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}
