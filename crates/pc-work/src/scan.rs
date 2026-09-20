//! `scan` — build the inventory and evaluate the safety gates.

use anyhow::Result;
use pc_core::{fmt_bytes, BlockReason, DerivedKind};
use pc_db::{Db, NewBundle, NewCatalog};
use pc_lightroom::CatalogReader;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Standard previews render at roughly three images per second on a modern
/// desktop CPU. Deliberately pessimistic: the hint exists to set expectations,
/// and an over-estimate is the harmless direction.
const PREVIEWS_PER_SECOND: i64 = 3;

fn rebuild_hint(file_count: i64) -> String {
    let minutes = (file_count / PREVIEWS_PER_SECOND / 60).max(1);
    if minutes < 60 {
        pc_core::tf!("~{0} мин пересборки", "~{0} min to rebuild", minutes)
    } else {
        pc_core::tf!(
            "~{0:.1} ч пересборки",
            "~{0:.1} h to rebuild",
            minutes as f64 / 60.0
        )
    }
}

struct CatalogInfo {
    is_locked: bool,
    file_count: Option<i64>,
    read_error: Option<String>,
}

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
    for e in result.errors.iter().take(20) {
        tracing::warn!("{e}");
    }
    if result.errors.len() > 20 {
        tracing::warn!("… и ещё {} ошибок доступа", result.errors.len() - 20);
    }

    // ---- catalogs ---------------------------------------------------------
    let mut catalogs: HashMap<String, CatalogInfo> = HashMap::new();
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

        catalogs.insert(
            key,
            CatalogInfo {
                is_locked: c.is_locked,
                file_count,
                read_error,
            },
        );
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

        let (block, hint) = evaluate(b.kind, owner_key.as_deref(), &catalogs);
        db.set_block(id, block.as_ref())?;
        db.set_rebuild_hint(id, hint.as_deref())?;
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

/// Decide whether a bundle may be removed, and what to tell the user about it.
fn evaluate(
    kind: DerivedKind,
    owner: Option<&str>,
    catalogs: &HashMap<String, CatalogInfo>,
) -> (Option<BlockReason>, Option<String>) {
    // Enforced by kind, not by policy: no gate can unlock it.
    if !kind.regenerable() {
        return (Some(BlockReason::NotRegenerable), None);
    }

    let Some(owner) = owner else {
        return (None, None);
    };

    let info = catalogs.get(owner);
    let owner_exists = Path::new(owner).exists();

    // Orphaned previews: the catalog they belong to is gone, so they can never
    // be used again by anything.
    if info.is_none() && !owner_exists {
        return (
            None,
            Some(
                pc_core::tr!(
                    "каталог не найден — сирота",
                    "catalogue not found — orphaned"
                )
                .to_string(),
            ),
        );
    }

    if let Some(info) = info {
        if info.is_locked {
            return (Some(BlockReason::CatalogOpen), None);
        }

        if kind == DerivedKind::LrSmartPreviews {
            // Smart previews are only regenerable while their masters are
            // reachable; with the originals offline they are the only editable
            // copy that exists.
            return match pc_lightroom::check_originals(Path::new(owner)) {
                Ok(c) if c.all_present() => (
                    None,
                    Some(pc_core::tf!(
                        "{0} оригиналов на месте",
                        "{0} originals all present",
                        c.total
                    )),
                ),
                Ok(c) => (
                    Some(BlockReason::OriginalsMissing {
                        missing: c.missing,
                        total: c.total,
                    }),
                    None,
                ),
                Err(e) => (
                    Some(BlockReason::OwnerUnreadable {
                        detail: e.to_string(),
                    }),
                    None,
                ),
            };
        }

        if kind == DerivedKind::LrPreviews {
            if let Some(n) = info.file_count {
                return (None, Some(rebuild_hint(n)));
            }
            if let Some(e) = &info.read_error {
                // Readable-ness of the catalog does not gate ordinary previews:
                // they are rebuildable regardless. Only the hint is lost.
                tracing::debug!("каталог {owner} не прочитан: {e}");
            }
        }
    }

    (None, None)
}

fn report(db: &Db) -> Result<()> {
    let all = db.list_bundles(&pc_db::model::BundleFilter::default())?;
    let removable: u64 = all
        .iter()
        .filter(|b| b.removable())
        .map(|b| b.size as u64)
        .sum();
    println!(
        "\nОпись готова: {} бандлов, к переносу пригодно {}.\n\
         Подробности: photo-cleanup derived list",
        all.len(),
        fmt_bytes(removable)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(is_locked: bool, file_count: Option<i64>) -> CatalogInfo {
        CatalogInfo {
            is_locked,
            file_count,
            read_error: None,
        }
    }

    #[test]
    fn catalog_data_is_always_blocked() {
        let (block, _) = evaluate(DerivedKind::LrCatalogData, None, &HashMap::new());
        assert_eq!(block, Some(BlockReason::NotRegenerable));
    }

    #[test]
    fn an_open_catalog_blocks_its_previews() {
        let mut m = HashMap::new();
        m.insert("/x/Family.lrcat".to_string(), cat(true, Some(9000)));
        let (block, _) = evaluate(DerivedKind::LrPreviews, Some("/x/Family.lrcat"), &m);
        assert_eq!(block, Some(BlockReason::CatalogOpen));
    }

    #[test]
    fn previews_get_a_rebuild_hint() {
        let mut m = HashMap::new();
        m.insert("/x/Family.lrcat".to_string(), cat(false, Some(9000)));
        let (block, hint) = evaluate(DerivedKind::LrPreviews, Some("/x/Family.lrcat"), &m);
        assert!(block.is_none());
        assert_eq!(hint.as_deref(), Some("~50 min to rebuild"));
    }

    #[test]
    fn orphaned_previews_are_removable() {
        let (block, hint) = evaluate(
            DerivedKind::LrPreviews,
            Some("/nowhere/Gone.lrcat"),
            &HashMap::new(),
        );
        assert!(block.is_none());
        assert!(hint.unwrap().contains("orphaned"));
    }

    #[test]
    fn rebuild_hint_scales_to_hours() {
        assert_eq!(rebuild_hint(180), "~1 min to rebuild");
        assert_eq!(rebuild_hint(9000), "~50 min to rebuild");
        assert!(rebuild_hint(100_000).contains("h to rebuild"));
    }
}
