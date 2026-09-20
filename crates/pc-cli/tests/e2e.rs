//! End-to-end cover for phase 0 on a tree shaped like the real archive:
//! live event catalogs, an open catalog, smart previews with missing masters,
//! an orphaned preview bundle, protected catalog data and real photos.
//!
//! The risky plumbing — quarantine, undo, purge — is what these tests exist
//! for: a mistake here costs photographs, not time.

use std::fs;
use std::path::{Path, PathBuf};

use pc_db::{BundleState, Db};

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![b'x'; bytes]).unwrap();
}

fn bundle_dir(path: &Path, files: usize) {
    for i in 0..files {
        write(&path.join("1/2").join(format!("preview{i}")), 512);
    }
}

fn catalog(path: &Path, root: &Path, rel: &str, files: &[&str]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE AgLibraryRootFolder(id_local INTEGER PRIMARY KEY, absolutePath TEXT);
         CREATE TABLE AgLibraryFolder(id_local INTEGER PRIMARY KEY, pathFromRoot TEXT, rootFolder INTEGER);
         CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, folder INTEGER, idx_filename TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO AgLibraryRootFolder VALUES (1, ?1)",
        [format!("{}/", root.display())],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO AgLibraryFolder VALUES (1, ?1, 1)",
        [format!("{rel}/")],
    )
    .unwrap();
    for (i, f) in files.iter().enumerate() {
        conn.execute(
            "INSERT INTO AgLibraryFile VALUES (?1, 1, ?2)",
            rusqlite::params![i as i64 + 1, f],
        )
        .unwrap();
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    foto: PathBuf,
    quarantine: PathBuf,
    db: Db,
}

fn build() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let foto = tmp.path().join("foto");
    let quarantine = tmp.path().join("quarantine");

    // A live event catalog with ordinary previews and a helper bundle.
    let dog = foto.join("Lightroom_lib/Dogshow");
    catalog(&dog.join("Dogshow.lrcat"), &dog, "raw", &["a.arw", "b.arw"]);
    write(&dog.join("raw/a.arw"), 10);
    write(&dog.join("raw/b.arw"), 10);
    bundle_dir(&dog.join("Dogshow Previews.lrdata"), 8);
    bundle_dir(&dog.join("Dogshow Helper.lrdata"), 2);

    // Catalog data: protected, must never be selectable.
    bundle_dir(&dog.join("Dogshow.lrcat-data"), 3);

    // Smart previews whose masters are partly gone.
    let work = foto.join("F/Lightroom Libraries/Work");
    catalog(
        &work.join("Work.lrcat"),
        &work,
        "masters",
        &["a.arw", "gone.arw"],
    );
    write(&work.join("masters/a.arw"), 10);
    bundle_dir(&work.join("Work Previews.lrdata"), 4);
    bundle_dir(&work.join("Work Smart Previews.lrdata"), 2);

    // An open catalog blocks everything it owns.
    let fam = foto.join("F/Lightroom Libraries/Family");
    catalog(&fam.join("Family.lrcat"), &fam, "masters", &["x.arw"]);
    write(&fam.join("masters/x.arw"), 10);
    write(&fam.join("Family.lrcat.lock"), 0);
    bundle_dir(&fam.join("Family Previews.lrdata"), 5);

    // Orphan: no catalog next to it.
    bundle_dir(&foto.join("E/Old/Gone Previews.lrdata"), 3);

    // Real photographs and a backup catalog that must be recognised as one.
    write(&foto.join("D/2019/DSC09999.ARW"), 4096);
    write(&foto.join("D/2019/DSC09999.JPG"), 2048);
    write(&foto.join(".DS_Store"), 0);
    catalog(
        &work.join("Backups/2015-06-01 2138/Work.lrcat"),
        &work,
        "masters",
        &["a.arw"],
    );

    let db = Db::open(&tmp.path().join("pc.db")).unwrap();
    pc_cli::scan::run(&db, std::slice::from_ref(&foto), "test").unwrap();

    Fixture {
        _tmp: tmp,
        foto,
        quarantine,
        db,
    }
}

fn by_name<'a>(bundles: &'a [pc_db::Bundle], needle: &str) -> &'a pc_db::Bundle {
    bundles
        .iter()
        .find(|b| b.path.contains(needle))
        .unwrap_or_else(|| panic!("нет бандла с «{needle}» среди {} найденных", bundles.len()))
}

fn all(db: &Db) -> Vec<pc_db::Bundle> {
    db.list_bundles(&pc_db::model::BundleFilter::default())
        .unwrap()
}

#[test]
fn gates_classify_every_bundle_correctly() {
    let fx = build();
    let bundles = all(&fx.db);

    let cat_data = by_name(&bundles, "Dogshow.lrcat-data");
    assert!(!cat_data.regenerable, "данные каталога не регенерируемы");
    assert!(!cat_data.removable());
    assert_eq!(cat_data.blocked_code.as_deref(), Some("not-regenerable"));

    let family = by_name(&bundles, "Family Previews.lrdata");
    assert_eq!(family.blocked_code.as_deref(), Some("catalog-open"));

    let smart = by_name(&bundles, "Work Smart Previews.lrdata");
    assert_eq!(smart.blocked_code.as_deref(), Some("originals-missing"));
    assert!(smart.blocked_detail.as_ref().unwrap().contains("1 of 2"));

    let orphan = by_name(&bundles, "Gone Previews.lrdata");
    assert!(orphan.removable(), "сирота подлежит переносу");

    let dog = by_name(&bundles, "Dogshow Previews.lrdata");
    assert!(dog.removable());
    assert!(dog
        .rebuild_cost_hint
        .as_ref()
        .unwrap()
        .contains("to rebuild"));
}

#[test]
fn catalog_backups_are_recognised_and_not_read() {
    let fx = build();
    let cats = fx.db.all_catalogs().unwrap();
    let backup = cats.iter().find(|c| c.path.contains("Backups")).unwrap();
    assert!(backup.is_backup);
    assert!(backup.image_count.is_none(), "бэкапы не открываются");

    let live = cats
        .iter()
        .find(|c| c.path.ends_with("Dogshow.lrcat"))
        .unwrap();
    assert_eq!(live.image_count, Some(2));
    assert!(!live.is_backup);
}

#[test]
fn preview_bundles_are_pruned_not_descended_into() {
    let fx = build();
    let bundles = all(&fx.db);
    // One row per bundle, not one per preview file inside it.
    let dog = by_name(&bundles, "Dogshow Previews.lrdata");
    assert_eq!(dog.file_count, 8);
    assert!(
        !bundles.iter().any(|b| b.path.contains("preview0")),
        "содержимое бандла не должно попадать в опись"
    );
}

#[test]
fn quarantine_moves_only_what_is_allowed_and_undo_restores_it() {
    let fx = build();
    let run_id = fx.db.latest_run().unwrap().unwrap();

    let removable = fx
        .db
        .list_bundles(&pc_db::model::BundleFilter {
            kind: Some(pc_core::DerivedKind::LrPreviews),
            state: Some(BundleState::Present),
            removable_only: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(removable.len(), 3, "Dogshow, Work и сирота");

    let totals =
        pc_apply::quarantine_many(&fx.db, run_id, &removable, Some(&fx.quarantine)).unwrap();
    assert_eq!(totals.bundles, 3);
    assert!(totals.skipped.is_empty(), "{:?}", totals.skipped);

    // Gone from the archive, present in quarantine.
    assert!(!fx
        .foto
        .join("Lightroom_lib/Dogshow/Dogshow Previews.lrdata")
        .exists());
    assert!(fx.quarantine.exists());

    // Everything protected is still exactly where it was.
    for keep in [
        "D/2019/DSC09999.ARW",
        "D/2019/DSC09999.JPG",
        "Lightroom_lib/Dogshow/Dogshow.lrcat",
        "Lightroom_lib/Dogshow/Dogshow.lrcat-data",
        "F/Lightroom Libraries/Family/Family Previews.lrdata",
        "F/Lightroom Libraries/Work/Work Smart Previews.lrdata",
        "F/Lightroom Libraries/Work/masters/a.arw",
    ] {
        assert!(fx.foto.join(keep).exists(), "пропало: {keep}");
    }

    // Undo puts every bundle back.
    let entries = fx.db.journal_quarantined(None).unwrap();
    assert_eq!(entries.len(), 3);
    for e in &entries {
        pc_apply::undo(&fx.db, e.id).unwrap();
        assert!(Path::new(&e.src).exists(), "не восстановлено: {}", e.src);
    }
    assert!(all(&fx.db).iter().all(|b| b.state == BundleState::Present));
}

#[test]
fn quarantine_refuses_a_destination_on_another_filesystem() {
    let fx = build();
    let bundles = all(&fx.db);
    let dog = by_name(&bundles, "Dogshow Previews.lrdata");
    // /dev is a different filesystem on both macOS and Linux.
    let err = pc_apply::quarantine_dest(dog, Some(Path::new("/dev"))).unwrap_err();
    assert!(err.to_string().contains("different filesystem"), "{err}");
}

#[test]
fn a_bundle_that_changed_since_the_scan_is_skipped() {
    let fx = build();
    let run_id = fx.db.latest_run().unwrap().unwrap();
    let bundles = all(&fx.db);
    let dog = by_name(&bundles, "Dogshow Previews.lrdata").clone();

    // Lightroom wrote a new preview after we scanned.
    write(
        &fx.foto
            .join("Lightroom_lib/Dogshow/Dogshow Previews.lrdata/1/2/extra"),
        99,
    );

    let out = pc_apply::quarantine(&fx.db, run_id, &dog, Some(&fx.quarantine)).unwrap();
    assert_eq!(out, pc_apply::Outcome::Skipped);
    assert!(fx
        .foto
        .join("Lightroom_lib/Dogshow/Dogshow Previews.lrdata")
        .exists());
}

#[test]
fn purge_deletes_only_after_the_retention_window() {
    let fx = build();
    let run_id = fx.db.latest_run().unwrap().unwrap();
    let bundles = all(&fx.db);
    let dog = by_name(&bundles, "Dogshow Previews.lrdata").clone();

    pc_apply::quarantine(&fx.db, run_id, &dog, Some(&fx.quarantine)).unwrap();

    // Nothing is old enough for a seven-day window yet.
    let kept = pc_apply::purge(&fx.db, 7 * 86_400).unwrap();
    assert_eq!(kept.bundles, 0);
    assert_eq!(pc_apply::quarantined_totals(&fx.db).unwrap().bundles, 1);

    // With a zero window it goes for good.
    let gone = pc_apply::purge(&fx.db, 0).unwrap();
    assert_eq!(gone.bundles, 1);
    assert_eq!(
        fx.db.bundle(dog.id).unwrap().unwrap().state,
        BundleState::Purged
    );
    assert!(
        pc_apply::undo(&fx.db, 1).is_err(),
        "откат после purge невозможен"
    );
}
