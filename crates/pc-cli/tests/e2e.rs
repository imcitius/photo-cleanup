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

#[path = "../../pc-core/src/derived/fixtures.rs"]
mod junk;
const EA_DIR: &str = "D/2019/@eaDir";

struct Fixture {
    _tmp: tempfile::TempDir,
    foto: PathBuf,
    db: Db,
}

fn build() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let foto = tmp.path().join("foto");

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
    // System files, none of which is recorded or moved (el-2rpxq): a
    // well-formed `.DS_Store`, an empty one, and two `@eaDir` folders — one
    // of `.DS_Store`s, one holding a JPEG.
    fs::write(foto.join(".DS_Store"), junk::ds_store()).unwrap();
    write(&foto.join("D/.DS_Store"), 0);
    fs::create_dir_all(foto.join("D/2019/@eaDir/DSC09999.JPG")).unwrap();
    fs::write(
        foto.join(EA_DIR).join("DSC09999.JPG/.DS_Store"),
        junk::ds_store(),
    )
    .unwrap();
    fs::create_dir_all(foto.join("E/@eaDir/x")).unwrap();
    fs::write(
        foto.join("E/@eaDir/x/SYNOPHOTO_THUMB_XL.jpg"),
        [0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10, b'J', b'F', b'I', b'F'],
    )
    .unwrap();
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

    // Nothing of Lightroom's is selectable, whatever its catalogue says:
    // open, closed, orphaned, smart previews or catalogue data (el-126jk).
    for name in [
        "Dogshow.lrcat-data",
        "Dogshow Previews.lrdata",
        "Dogshow Helper.lrdata",
        "Family Previews.lrdata",
        "Work Previews.lrdata",
        "Work Smart Previews.lrdata",
        "Gone Previews.lrdata",
    ] {
        let b = by_name(&bundles, name);
        assert_eq!(b.blocked_code.as_deref(), Some("lightroom"), "{name}");
        assert!(b.refusal().contains("Lightroom is never touched"));
    }
    assert!(!by_name(&bundles, "Dogshow.lrcat-data").regenerable);

    // No system file is recorded, proven bytes or not (el-2rpxq): `derived
    // clean` would move none of it, and `@eaDir` is pruned unrecorded.
    for b in &bundles {
        assert_ne!(b.kind, pc_core::DerivedKind::SystemJunk, "{}", b.path);
        assert!(!b.path.contains("@eaDir"), "{}", b.path);
    }
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

/// el-126jk, el-2rpxq, el-1bzcw: `derived clean` moves nothing — every
/// present bundle of every kind is named as left, with its reason, and
/// there is no move for a bundle to hand it to. Undo and purge of what an
/// earlier version moved: pc-apply's purge and Lightroom tests.
#[test]
fn derived_clean_leaves_every_bundle_and_says_why() {
    let fx = build();
    let kinds = [
        pc_core::DerivedKind::LrPreviews,
        pc_core::DerivedKind::LrSmartPreviews,
        pc_core::DerivedKind::LrHelper,
        pc_core::DerivedKind::LrDataOther,
        pc_core::DerivedKind::LrCatalogData,
        pc_core::DerivedKind::SystemJunk,
    ];
    let sel = pc_apply::select_derived(&fx.db, &kinds, None).unwrap();
    let present: Vec<_> = all(&fx.db)
        .into_iter()
        .filter(|b| b.state == BundleState::Present)
        .collect();
    assert!(!present.is_empty());
    assert_eq!(sel.excluded.len(), present.len(), "{:?}", sel.excluded);
    for b in &present {
        let (_, why) = sel
            .excluded
            .iter()
            .find(|(p, _)| *p == b.path)
            .unwrap_or_else(|| panic!("{} not named", b.path));
        assert_eq!(*why, b.refusal());
    }
    assert!(fx.db.journal_quarantined(None).unwrap().is_empty());

    for keep in [
        ".DS_Store",
        "D/2019/DSC09999.ARW",
        "D/2019/DSC09999.JPG",
        "D/.DS_Store",
        "D/2019/@eaDir/DSC09999.JPG/.DS_Store",
        "E/@eaDir/x/SYNOPHOTO_THUMB_XL.jpg",
        "Lightroom_lib/Dogshow/Dogshow.lrcat",
        "Lightroom_lib/Dogshow/Dogshow.lrcat-data",
        "Lightroom_lib/Dogshow/Dogshow Previews.lrdata",
        "Lightroom_lib/Dogshow/Dogshow Helper.lrdata",
        "E/Old/Gone Previews.lrdata",
        "F/Lightroom Libraries/Family/Family Previews.lrdata",
        "F/Lightroom Libraries/Work/Work Previews.lrdata",
        "F/Lightroom Libraries/Work/Work Smart Previews.lrdata",
        "F/Lightroom Libraries/Work/masters/a.arw",
    ] {
        assert!(fx.foto.join(keep).exists(), "пропало: {keep}");
    }
}
