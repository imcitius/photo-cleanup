//! `scan` — build the inventory and evaluate the safety gates.

use anyhow::Result;
use pc_core::fmt_bytes;
use pc_db::{Db, NewBundle, NewCatalog};
use pc_lightroom::CatalogReader;
use std::path::PathBuf;

pub fn run(db: &Db, roots: &[PathBuf], version: &str) -> Result<()> {
    run_controlled(db, roots, version, &pc_core::work::Control::default())
}
pub fn run_controlled(
    db: &Db,
    roots: &[PathBuf],
    version: &str,
    control: &pc_core::work::Control,
) -> Result<()> {
    let root_strings: Vec<String> = roots.iter().map(|p| p.display().to_string()).collect();
    let run_id = db.start_run(&root_strings, version)?;

    let result = pc_walk::scan_controlled(roots, &pc_walk::Options::default(), control)?;
    tracing::info!(
        dirs = result.dirs_visited,
        bundles = result.bundles.len(),
        catalogs = result.catalogs.len(),
        "обход завершён"
    );
    // What the walk stepped over: our own quarantine folders. Written down
    // so the interface can say what is in them, even when the journal that
    // put them there belonged to a database that is gone.
    let quarantined: Vec<(String, i64, i64)> = result
        .quarantined
        .iter()
        .map(|q| (q.path.display().to_string(), q.size as i64, q.mtime))
        .collect();
    db.set_quarantine_found(run_id, &quarantined)?;

    for e in result.errors.iter().take(20) {
        tracing::warn!("{e}");
    }
    if result.errors.len() > 20 {
        tracing::warn!("… и ещё {} ошибок доступа", result.errors.len() - 20);
    }

    // ---- catalogs ---------------------------------------------------------
    control.begin(
        pc_core::tr!("Чтение каталогов Lightroom", "Reading Lightroom catalogues"),
        result.catalogs.len() as u64,
        0,
    )?;
    for c in &result.catalogs {
        control.current(&c.path.display().to_string())?;
        control.advance(0, None);
        let key = c.path.display().to_string();
        let mut file_count = None;
        let mut read_error = None;

        // A backup catalog is a zip-era artefact or a copy; reading it tells us
        // nothing useful and it never owns a live preview bundle.
        let mut entries = Vec::new();
        if !c.is_backup {
            match CatalogReader::open(&c.path).and_then(|r| r.entries()) {
                Ok(list) => {
                    file_count = Some(list.len() as i64);
                    entries = list;
                }
                Err(e) => read_error = Some(e.to_string()),
            }
        }

        let catalog_id = db.upsert_catalog(&NewCatalog {
            path: key.clone(),
            name: c.name.clone(),
            disk: c.disk.label.clone(),
            size: c.size as i64,
            is_backup: c.is_backup,
            is_locked: c.is_locked,
            image_count: file_count,
            read_error: read_error.clone(),
        })?;

        // Remember which frames the photographer has curated, so the plan
        // can refuse to propose them for deletion.
        if !entries.is_empty() {
            let rows: Vec<(String, Option<i64>, Option<i64>)> = entries
                .into_iter()
                .map(|e| (e.path, e.rating, e.pick))
                .collect();
            db.replace_catalog_files(catalog_id, &rows)?;
        }
    }

    // ---- bundles and gates ------------------------------------------------
    control.begin(
        pc_core::tr!("Опись производных данных", "Taking stock of derived data"),
        result.bundles.len() as u64,
        0,
    )?;
    for b in &result.bundles {
        control.current(&b.path.display().to_string())?;
        control.advance(b.size, Some(&b.disk.label));
        let owner_key = b.owner_ref.as_ref().map(|p| p.display().to_string());

        let id = db.upsert_bundle(
            &NewBundle {
                path: b.path.display().to_string(),
                is_dir: b.is_dir,
                disk: b.disk.label.clone(),
                dev: b.disk.dev as i64,
                mount: b.disk.mount.display().to_string(),
                kind: b.kind,
                owner_ref: owner_key.clone(),
                file_count: b.file_count as i64,
                size: b.size as i64,
                newest_mtime: b.newest_mtime,
            },
            run_id,
        )?;

        // Nothing derived is moved (el-126jk): every bundle is kept, with
        // the reason shown, and asked again at the moment of any move.
        let block = pc_core::derived::refusal(b.kind, &b.path);
        db.set_block(id, Some(&block))?;
        db.set_rebuild_hint(id, None)?;
    }

    for e in &result.errors {
        control.refuse(
            e,
            pc_core::tr!("Ошибка доступа при обходе", "Access error during the walk"),
        );
    }
    db.finish_run(run_id)?;
    report(db)?;
    Ok(())
}

fn report(db: &Db) -> Result<()> {
    let all = db.list_bundles(&pc_db::model::BundleFilter::default())?;
    // Nothing in the inventory can be moved (el-126jk, el-2rpxq); the line
    // keeps its old shape, at zero.
    println!(
        "\nInventory done: {} bundles, {} of it can be moved.\n\
         Details: photo-cleanup derived list",
        all.len(),
        fmt_bytes(0)
    );
    Ok(())
}
