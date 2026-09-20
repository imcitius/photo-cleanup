//! The path that actually removes photographs, end to end.
//!
//! Everything here is about what must *not* happen: the original stays, a
//! curated frame stays, a family is never emptied, and a file whose pixels
//! no longer match what we are keeping is refused at the last moment.

use std::fs;
use std::path::{Path, PathBuf};

use image::{DynamicImage, Rgb, RgbImage};
use pc_db::Db;
use pc_family::{plan, Policy, Role};

/// A photograph-like image: low-frequency content that survives rescaling,
/// distinct per seed.
fn photo(w: u32, h: u32, seed: u32) -> DynamicImage {
    let s = seed as f32;
    let mut img = RgbImage::new(w, h);
    for (x, y, p) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let tau = std::f32::consts::TAU;
        let a = ((fx * (2.0 + s * 0.7) + s * 0.3) * tau).sin();
        let b = ((fy * (1.6 + s * 0.4) + s * 0.8) * tau).cos();
        let v = |k: f32| ((128.0 + (a + b) * 45.0 * k).clamp(0.0, 255.0)) as u8;
        *p = Rgb([v(1.0), v(0.85), v(0.6)]);
    }
    DynamicImage::ImageRgb8(img)
}

fn write_jpeg(path: &Path, img: &DynamicImage, quality: u8) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rgb = img.to_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    fs::write(path, bytes).unwrap();
}

struct World {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    quarantine: PathBuf,
    db: Db,
}

fn build_world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("foto");
    let quarantine = tmp.path().join("q");

    // One photograph in three places: the original and two exact copies.
    let a = photo(1400, 1000, 3);
    write_jpeg(&root.join("2019/DSC01234.JPG"), &a, 92);
    let bytes = fs::read(root.join("2019/DSC01234.JPG")).unwrap();
    fs::create_dir_all(root.join("Backup/2019")).unwrap();
    fs::write(root.join("Backup/2019/DSC01234.JPG"), &bytes).unwrap();
    fs::create_dir_all(root.join("Telegram")).unwrap();
    fs::write(root.join("Telegram/DSC01234.JPG"), &bytes).unwrap();
    // A sidecar that must travel with the copy it belongs to.
    fs::write(root.join("Telegram/DSC01234.xmp"), b"<x/>").unwrap();

    // A different photograph, curated in Lightroom, with an exact copy.
    let b = photo(1400, 1000, 17);
    write_jpeg(&root.join("2020/DSC05555.JPG"), &b, 92);
    let bb = fs::read(root.join("2020/DSC05555.JPG")).unwrap();
    fs::create_dir_all(root.join("Dump")).unwrap();
    fs::write(root.join("Dump/DSC05555.JPG"), &bb).unwrap();

    // An unrelated photograph that must stay a family of one.
    write_jpeg(&root.join("2021/DSC07777.JPG"), &photo(1400, 1000, 31), 92);

    // A Lightroom catalog that has curated the copy sitting in Dump.
    let cat = root.join("Cat/Work.lrcat");
    fs::create_dir_all(cat.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&cat).unwrap();
    conn.execute_batch(
        "CREATE TABLE AgLibraryRootFolder(id_local INTEGER PRIMARY KEY, absolutePath TEXT);
         CREATE TABLE AgLibraryFolder(id_local INTEGER PRIMARY KEY, pathFromRoot TEXT, rootFolder INTEGER);
         CREATE TABLE AgLibraryFile(id_local INTEGER PRIMARY KEY, folder INTEGER, idx_filename TEXT);
         CREATE TABLE Adobe_images(id_local INTEGER PRIMARY KEY, rootFile INTEGER, rating REAL, pick REAL, fileFormat TEXT);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO AgLibraryRootFolder VALUES (1, ?1)",
        [format!("{}/", root.join("Dump").display())],
    )
    .unwrap();
    // Both copies are in the catalog, as happens when the same frame was
    // imported from two folders. Whichever loses the keeper contest is then
    // a removal candidate that the protection has to refuse.
    conn.execute(
        "INSERT INTO AgLibraryRootFolder VALUES (2, ?1)",
        [format!("{}/", root.join("2020").display())],
    )
    .unwrap();
    conn.execute_batch(
        "INSERT INTO AgLibraryFolder VALUES (1, '', 1), (2, '', 2);
         INSERT INTO AgLibraryFile   VALUES (1, 1, 'DSC05555.JPG'), (2, 2, 'DSC05555.JPG');
         INSERT INTO Adobe_images    VALUES (1, 1, 5.0, 1.0, 'JPG'), (2, 2, 4.0, NULL, 'JPG');",
    )
    .unwrap();
    drop(conn);

    let db = Db::open(&tmp.path().join("pc.db")).unwrap();
    let store = pc_core::ThumbStore::new(tmp.path().join("thumbs"));

    pc_cli::scan::run(&db, std::slice::from_ref(&root), "test").unwrap();
    pc_cli::index::run(
        &db,
        std::slice::from_ref(&root),
        &store,
        &pc_cli::index::Options {
            min_file_size: 1024,
            ..Default::default()
        },
    )
    .unwrap();
    pc_family::build(&db, &store, &pc_family::Params::default()).unwrap();

    World {
        _tmp: tmp,
        root,
        quarantine,
        db,
    }
}

/// Paths are written with the platform's own separator, and these tests read
/// them by their tail. Comparing on one spelling keeps the assertions about
/// the archive rather than about the operating system.
fn tail(path: &str, ending: &str) -> bool {
    path.replace('\\', "/").ends_with(ending)
}

fn plan_with(w: &World, policy: &Policy) -> plan::Plan {
    plan::compute(&w.db, policy).unwrap()
}

#[test]
fn exact_copies_are_found_and_the_original_is_not_among_them() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());

    let paths: Vec<&str> = p.candidates.iter().map(|c| c.path.as_str()).collect();
    assert!(
        paths.iter().any(|x| tail(x, "Telegram/DSC01234.JPG")),
        "копия в Telegram не найдена: {paths:?}"
    );
    assert!(
        paths.iter().any(|x| tail(x, "Backup/2019/DSC01234.JPG")),
        "копия в Backup не найдена: {paths:?}"
    );
    // The one in its proper place is what the others are measured against.
    assert!(
        !paths.iter().any(|x| tail(x, "foto/2019/DSC01234.JPG")),
        "предложен сам оригинал: {paths:?}"
    );
    for c in &p.candidates {
        assert!(
            tail(&c.keeper_path, "foto/2019/DSC01234.JPG") || c.keeper_path.contains("DSC05555"),
            "сохраняется не тот файл: {}",
            c.keeper_path
        );
    }
    for c in &p.candidates {
        assert_eq!(c.role, Role::Copy, "по умолчанию только точные копии");
        assert_ne!(c.path, c.keeper_path);
    }
}

#[test]
fn a_backup_folder_never_wins_the_contest_against_the_working_copy() {
    // Byte-identical files score identically, so the tie-break decides. If
    // it were arbitrary the tool could move the file in its proper place and
    // keep the one buried in a backup — silently, and differently each run.
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    let c = p
        .candidates
        .iter()
        .find(|c| tail(&c.path, "Backup/2019/DSC01234.JPG"))
        .expect("копия в Backup не предложена");
    assert!(tail(&c.keeper_path, "foto/2019/DSC01234.JPG"));
}

#[test]
fn a_frame_a_lightroom_catalog_curates_is_refused() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());

    // Both copies of DSC05555 are catalogued; one is the keeper and the
    // other must be refused rather than moved.
    let refused = p
        .refusals
        .iter()
        .find(|r| r.path.contains("DSC05555"))
        .unwrap_or_else(|| panic!("защита Lightroom не сработала: {:?}", p.refusals));
    assert!(refused.why.contains("Lightroom"), "{}", refused.why);
    assert!(
        refused.why.contains("stars"),
        "рейтинг не попал в причину: {}",
        refused.why
    );
    assert!(
        !p.candidates.iter().any(|c| c.path.contains("DSC05555")),
        "каталогизированный кадр попал в план"
    );
}

#[test]
fn lifting_the_protection_deliberately_makes_it_a_candidate() {
    let w = build_world();
    let p = plan_with(
        &w,
        &Policy {
            respect_lightroom: false,
            ..Default::default()
        },
    );
    assert!(
        p.candidates.iter().any(|c| c.path.contains("DSC05555")),
        "снятие защиты ничего не изменило"
    );
}

#[test]
fn a_photograph_with_no_duplicate_is_never_proposed() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    assert!(!p.candidates.iter().any(|c| c.path.contains("DSC07777")));
}

#[test]
fn applying_moves_the_copies_and_leaves_everything_else_alone() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    let run = w.db.latest_run().unwrap().unwrap();

    let report = pc_apply::apply(&w.db, run, &p.candidates, Some(&w.quarantine)).unwrap();
    assert!(report.refused.is_empty(), "{:?}", report.refused);
    assert_eq!(report.totals.files, p.candidates.len() as u64);

    // The photograph itself is still there, in every role that was kept.
    for keep in [
        "2019/DSC01234.JPG",
        "2020/DSC05555.JPG",
        "2021/DSC07777.JPG",
        "Dump/DSC05555.JPG",
    ] {
        assert!(w.root.join(keep).exists(), "пропало: {keep}");
    }
    // The copies, and only the copies, are gone from the archive.
    assert!(!w.root.join("Backup/2019/DSC01234.JPG").exists());
    assert!(!w.root.join("Telegram/DSC01234.JPG").exists());

    // The sidecar went with its photograph rather than being orphaned.
    assert!(!w.root.join("Telegram/DSC01234.xmp").exists());
    let moved_sidecars = walkdir::WalkDir::new(&w.quarantine)
        .into_iter()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "xmp"))
        .count();
    assert_eq!(moved_sidecars, 1, "сайдкар остался сиротой");
}

#[test]
fn undo_puts_every_moved_photograph_back() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    let run = w.db.latest_run().unwrap().unwrap();
    pc_apply::apply(&w.db, run, &p.candidates, Some(&w.quarantine)).unwrap();

    let entries = w.db.journal_quarantined(None).unwrap();
    assert!(!entries.is_empty());
    for e in &entries {
        pc_apply::undo(&w.db, e.id).unwrap();
        assert!(Path::new(&e.src).exists(), "не вернулось: {}", e.src);
    }
}

#[test]
fn a_file_edited_since_the_scan_is_refused_at_the_last_moment() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    let run = w.db.latest_run().unwrap().unwrap();

    // Someone replaced the copy with a different photograph after we planned.
    let victim = p
        .candidates
        .iter()
        .find(|c| c.path.contains("Telegram"))
        .expect("нет кандидата в Telegram");
    write_jpeg(Path::new(&victim.path), &photo(1400, 1000, 61), 92);

    let report = pc_apply::apply(
        &w.db,
        run,
        std::slice::from_ref(victim),
        Some(&w.quarantine),
    )
    .unwrap();
    assert_eq!(report.totals.files, 0, "перенесён изменившийся файл");
    assert!(
        report.refused[0].1.contains("pixels"),
        "{:?}",
        report.refused
    );
    assert!(Path::new(&victim.path).exists());
}

#[test]
fn the_keeper_disappearing_stops_the_move() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    let run = w.db.latest_run().unwrap().unwrap();

    let victim = p
        .candidates
        .iter()
        .find(|c| c.path.contains("Telegram"))
        .unwrap()
        .clone();
    fs::remove_file(&victim.keeper_path).unwrap();

    let report = pc_apply::apply(
        &w.db,
        run,
        std::slice::from_ref(&victim),
        Some(&w.quarantine),
    )
    .unwrap();
    assert_eq!(report.totals.files, 0);
    assert!(
        Path::new(&victim.path).exists(),
        "снимок удалён без запасного"
    );
}

#[test]
fn the_plan_empties_out_once_it_has_been_applied() {
    // Without this the totals never go down: the rows still claim the files
    // are in the archive, so the same work is offered again after every run.
    let w = build_world();
    let first = plan_with(&w, &Policy::default());
    assert!(!first.candidates.is_empty());

    let run = w.db.latest_run().unwrap().unwrap();
    pc_apply::apply(&w.db, run, &first.candidates, Some(&w.quarantine)).unwrap();

    let second = plan_with(&w, &Policy::default());
    assert!(
        second.candidates.is_empty(),
        "после переноса план всё ещё предлагает {} файлов",
        second.candidates.len()
    );
}

#[test]
fn an_undone_move_puts_the_file_back_into_the_plan() {
    let w = build_world();
    let p = plan_with(&w, &Policy::default());
    let run = w.db.latest_run().unwrap().unwrap();
    pc_apply::apply(&w.db, run, &p.candidates, Some(&w.quarantine)).unwrap();

    for e in w.db.journal_quarantined(None).unwrap() {
        pc_apply::undo(&w.db, e.id).unwrap();
    }
    let again = plan_with(&w, &Policy::default());
    assert_eq!(
        again.candidates.len(),
        p.candidates.len(),
        "вернувшиеся файлы не попали обратно в план"
    );
}
